// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Serving until the process is asked to stop, then the bounded drain.
//!
//! A container runtime stops a container with `SIGTERM` to PID 1 and kills
//! it after a grace period, so the server answers the signal itself. On the
//! signal readiness turns `503` at once, the drain delay passes while the
//! listener still accepts (so an orchestrator stops routing first), the
//! listener closes, and the requests in flight get the shutdown bound to
//! finish; a connection still open then is dropped.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use ferrohealth_health::lifecycle::Lifecycle;
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// How the drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drained {
    /// Every request in flight finished inside the bound.
    Complete,
    /// The bound passed with connections still open, and they were dropped.
    Cut,
}

/// Serves `app` on `listener` until `signal` completes, then drains.
///
/// `lifecycle` moves to serving when this starts and to draining on the
/// signal. `delay` is the drain delay and `bound` the time the requests in
/// flight get once the listener closed.
///
/// # Errors
///
/// Returns the I/O error from accepting or serving connections.
pub async fn serve<F>(
    listener: TcpListener,
    app: Router,
    lifecycle: Lifecycle,
    signal: F,
    delay: Duration,
    bound: Duration,
) -> std::io::Result<Drained>
where
    F: Future<Output = ()> + Send + 'static,
{
    let closing = Arc::new(Notify::new());
    let closed = Arc::clone(&closing);
    let draining = lifecycle.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        signal.await;
        draining.drain();
        tracing::info!(
            drain_delay_ms = delay.as_millis(),
            "readiness withdrawn; the listener accepts until the drain delay ends"
        );
        tokio::time::sleep(delay).await;
        tracing::info!("the listener stops accepting; draining the requests in flight");
        closed.notify_one();
    });
    let server = server.into_future();
    tokio::pin!(server);
    lifecycle.booted();
    tokio::select! {
        outcome = &mut server => return outcome.map(|()| Drained::Complete),
        () = closing.notified() => {}
    }
    if let Ok(outcome) = tokio::time::timeout(bound, server).await {
        outcome.map(|()| Drained::Complete)
    } else {
        tracing::warn!(
            shutdown_timeout_ms = bound.as_millis(),
            "the drain bound passed; the connections still open are dropped"
        );
        Ok(Drained::Cut)
    }
}

/// How long a client may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves `app` over TLS on `listener` until `signal` completes, then
/// drains, as [`serve`] does over plain HTTP.
///
/// Each connection's TLS handshake is bounded by [`HANDSHAKE_TIMEOUT`]; a
/// failed handshake drops that connection alone. HTTP/2 and HTTP/1.1 are
/// served as the client chose by ALPN.
///
/// # Errors
///
/// Never fails today: an accept error is logged and the listener goes on.
/// The `Result` keeps the shape of [`serve`].
pub async fn serve_tls<F>(
    listener: TcpListener,
    app: Router,
    tls: Arc<rustls::ServerConfig>,
    lifecycle: Lifecycle,
    signal: F,
    delay: Duration,
    bound: Duration,
) -> std::io::Result<Drained>
where
    F: Future<Output = ()> + Send + 'static,
{
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    lifecycle.booted();
    tokio::pin!(signal);
    loop {
        tokio::select! {
            accepted = listener.accept() => accept(accepted, &acceptor, &app, &builder, &graceful),
            () = &mut signal => break,
        }
    }
    lifecycle.drain();
    tracing::info!(
        drain_delay_ms = delay.as_millis(),
        "readiness withdrawn; the listener accepts until the drain delay ends"
    );
    let delayed = tokio::time::sleep(delay);
    tokio::pin!(delayed);
    loop {
        tokio::select! {
            accepted = listener.accept() => accept(accepted, &acceptor, &app, &builder, &graceful),
            () = &mut delayed => break,
        }
    }
    drop(listener);
    tracing::info!("the listener stops accepting; draining the requests in flight");
    if tokio::time::timeout(bound, graceful.shutdown())
        .await
        .is_ok()
    {
        Ok(Drained::Complete)
    } else {
        tracing::warn!(
            shutdown_timeout_ms = bound.as_millis(),
            "the drain bound passed; the connections still open are dropped"
        );
        Ok(Drained::Cut)
    }
}

/// Serves one accepted connection over TLS on its own task.
fn accept(
    accepted: std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>,
    acceptor: &tokio_rustls::TlsAcceptor,
    app: &Router,
    builder: &hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
    graceful: &hyper_util::server::graceful::GracefulShutdown,
) {
    let stream = match accepted {
        Ok((stream, _peer)) => stream,
        Err(error) => {
            tracing::warn!(%error, "a connection could not be accepted");
            return;
        }
    };
    let acceptor = acceptor.clone();
    let service = hyper_util::service::TowerToHyperService::new(app.clone());
    let builder = builder.clone();
    let watcher = graceful.watcher();
    tokio::spawn(async move {
        let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                tracing::debug!(%error, "a TLS handshake failed");
                return;
            }
            Err(_elapsed) => {
                tracing::debug!("a TLS handshake did not finish in time");
                return;
            }
        };
        let connection = builder
            .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service)
            .into_owned();
        if let Err(error) = watcher.watch(connection).await {
            tracing::debug!(error = %error, "a TLS connection ended with an error");
        }
    });
}

/// Serves `app` as `section` configures it, over TLS when `[server.tls]`
/// is set, until `signal` completes, then drains.
///
/// # Errors
///
/// Returns [`ServeError::Tls`] when the TLS files cannot be read and
/// [`ServeError::Io`] for an I/O error of the listener.
pub async fn run<F>(
    listener: TcpListener,
    app: Router,
    section: &crate::section::ServerSection,
    lifecycle: Lifecycle,
    signal: F,
) -> Result<Drained, ServeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (delay, bound) = (section.drain_delay(), section.shutdown_timeout());
    match &section.tls {
        Some(tls) => {
            let config = crate::tls::server_config(tls)?;
            Ok(serve_tls(listener, app, config, lifecycle, signal, delay, bound).await?)
        }
        None => Ok(serve(listener, app, lifecycle, signal, delay, bound).await?),
    }
}

/// The server could not serve.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// The listener's TLS could not be set up.
    #[error(transparent)]
    Tls(#[from] crate::tls::TlsError),
    /// The listener failed.
    #[error("the listener failed")]
    Io(#[from] std::io::Error),
}

/// Completes when the process receives `SIGTERM` or `SIGINT`.
///
/// A handler that cannot be installed is logged and its signal never
/// completes, so the server keeps serving and the runtime's kill after its
/// grace period stays the backstop.
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };
    let terminate = async {
        #[cfg(unix)]
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
        #[cfg(not(unix))]
        std::future::pending::<()>().await;
    };
    tokio::select! {
        () = interrupt => tracing::info!("SIGINT received, shutting down"),
        () = terminate => tracing::info!("SIGTERM received, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::routing::get;
    use ferrohealth_health::document::Phase;
    use ferrohealth_health::lifecycle::Lifecycle;
    use ferrohealth_health::registry::Registry;
    use tokio::net::TcpListener;

    use super::{Drained, serve};
    use crate::health::Health;
    use crate::healthcheck::{Outcome, check};
    use crate::section::ServerSection;
    use crate::stack::{StackPolicy, app};

    async fn start(
        product: Router,
        delay: Duration,
        bound: Duration,
    ) -> (
        std::net::SocketAddr,
        Lifecycle,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<std::io::Result<Drained>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
        let app = app(
            product,
            Health::new(Registry::default(), lifecycle.clone()),
            &ServerSection::default(),
            StackPolicy::default(),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let signal = async move {
            let _received = stopped.await;
        };
        let served = tokio::spawn(serve(
            listener,
            app,
            lifecycle.clone(),
            signal,
            delay,
            bound,
        ));
        (address, lifecycle, stop, served)
    }

    #[tokio::test]
    async fn the_signal_withdraws_readiness_then_the_drain_completes() {
        let (address, lifecycle, stop, served) = start(
            Router::new(),
            Duration::from_millis(400),
            Duration::from_secs(5),
        )
        .await;
        let healthy = check(address, "", Duration::from_secs(2)).await;
        assert_eq!(Outcome::Healthy, healthy);
        assert_eq!(Phase::Serving, lifecycle.phase());
        stop.send(()).expect("the server listens for the signal");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(Phase::Draining, lifecycle.phase());
        let during = check(address, "", Duration::from_secs(2)).await;
        assert_eq!(
            Outcome::Unhealthy(503),
            during,
            "readiness is withdrawn during the delay"
        );
        let drained = served
            .await
            .expect("the task ends")
            .expect("the server serves");
        assert_eq!(Drained::Complete, drained);
    }

    struct Certificate {
        _dir: tempfile::TempDir,
        section: crate::section::TlsSection,
    }

    fn certificate() -> Certificate {
        let generated = rcgen::generate_simple_self_signed(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])
        .expect("a certificate is generated");
        let dir = tempfile::tempdir().expect("a directory");
        let certificate_file = dir.path().join("server.pem");
        let key_file = dir.path().join("server.key");
        std::fs::write(&certificate_file, generated.cert.pem())
            .expect("the certificate is written");
        std::fs::write(&key_file, generated.signing_key.serialize_pem())
            .expect("the key is written");
        Certificate {
            _dir: dir,
            section: crate::section::TlsSection {
                certificate_file,
                key_file,
            },
        }
    }

    #[tokio::test]
    async fn a_tls_listener_serves_the_pinned_healthcheck_and_drains() {
        let certificate = certificate();
        let config = crate::tls::server_config(&certificate.section).expect("the TLS files read");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
        let app = app(
            Router::new(),
            Health::new(Registry::default(), lifecycle.clone()),
            &ServerSection::default(),
            StackPolicy::default(),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let signal = async move {
            let _received = stopped.await;
        };
        let served = tokio::spawn(super::serve_tls(
            listener,
            app,
            config,
            lifecycle.clone(),
            signal,
            Duration::ZERO,
            Duration::from_secs(5),
        ));
        let pinned = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
        assert_eq!(
            Outcome::Healthy,
            crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &pinned).await
        );
        let plain = check(address, "", Duration::from_secs(2)).await;
        assert!(
            !matches!(plain, Outcome::Healthy),
            "plain HTTP to a TLS listener is no answer: {plain:?}"
        );
        let other = crate::tls::HealthcheckTls::read(&certificate_section_of_another())
            .expect("another pin reads");
        let refused =
            crate::healthcheck::check_tls(address, "", Duration::from_secs(5), &other).await;
        assert!(
            matches!(refused, Outcome::Failed(ref reason) if reason.contains("TLS")),
            "{refused:?}"
        );
        stop.send(()).expect("the server listens for the signal");
        let drained = served
            .await
            .expect("the task ends")
            .expect("the server serves");
        assert_eq!(Drained::Complete, drained);
        assert_eq!(Phase::Draining, lifecycle.phase());
    }

    fn certificate_section_of_another() -> crate::section::TlsSection {
        let other = certificate();
        let dir = tempfile::tempdir().expect("a directory").keep();
        let certificate_file = dir.join("other.pem");
        std::fs::copy(&other.section.certificate_file, &certificate_file)
            .expect("the certificate is copied");
        crate::section::TlsSection {
            certificate_file,
            key_file: dir.join("unused.key"),
        }
    }

    #[test]
    fn a_tls_file_that_is_missing_or_holds_no_pem_is_refused() {
        let certificate = certificate();
        let missing = crate::section::TlsSection {
            certificate_file: "/nonexistent/server.pem".into(),
            ..certificate.section.clone()
        };
        assert!(matches!(
            crate::tls::server_config(&missing),
            Err(crate::tls::TlsError::Read { .. })
        ));
        let swapped = crate::section::TlsSection {
            certificate_file: certificate.section.key_file.clone(),
            key_file: certificate.section.certificate_file.clone(),
        };
        assert!(matches!(
            crate::tls::server_config(&swapped),
            Err(crate::tls::TlsError::Pem { .. })
        ));
    }

    #[tokio::test]
    async fn a_request_still_running_past_the_bound_is_cut() {
        let product = Router::new().route(
            "/hold",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "held"
            }),
        );
        let (address, _lifecycle, stop, served) =
            start(product, Duration::ZERO, Duration::from_millis(300)).await;
        let held = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let mut stream = tokio::net::TcpStream::connect(address)
                .await
                .expect("a connection");
            stream
                .write_all(b"GET /hold HTTP/1.1\r\nhost: localhost\r\n\r\n")
                .await
                .expect("the request is sent");
            let mut buffer = Vec::new();
            let _read = stream.read_to_end(&mut buffer).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.send(()).expect("the server listens for the signal");
        let drained = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the drain is bounded")
            .expect("the task ends")
            .expect("the server serves");
        assert_eq!(Drained::Cut, drained);
        held.abort();
    }
}
