// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Serving until the process is asked to stop, then the bounded drain.
//!
//! A container runtime stops a container with `SIGTERM` to PID 1 and kills
//! it after a grace period, so the server answers the signal itself. On the
//! signal readiness turns `503` at once, the drain delay passes while the
//! listener still accepts (so an orchestrator stops routing first), the
//! listener closes, and the requests in flight get the shutdown bound to
//! finish; a connection still open then is closed before the call returns.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use ferrohealth_health::lifecycle::Lifecycle;
use tokio::net::TcpListener;

/// How the drain ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drained {
    /// Every request in flight finished inside the bound.
    Complete,
    /// The bound passed with connections still open, and they were closed.
    Cut,
}

/// Serves `app` on `listener` until `signal` completes, then drains.
///
/// `lifecycle` moves to serving when this starts and to draining on the
/// signal. `delay` is the drain delay and `bound` the time the requests in
/// flight get once the listener closed. A client gets
/// [`HEADER_READ_TIMEOUT`] to send a request head; [`run`] takes the bound
/// from the section.
///
/// # Errors
///
/// Never fails today: an accept error is logged and the listener goes on.
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
    let connections = Connections::new(app, None, HEADER_READ_TIMEOUT);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// How long a client may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send a request head when the caller names
/// no bound.
///
/// It is hyper's own default for HTTP/1
/// (<https://docs.rs/hyper/1/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout>).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

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
    tls: Arc<crate::tls::ReloadableTls>,
    lifecycle: Lifecycle,
    signal: F,
    delay: Duration,
    bound: Duration,
) -> std::io::Result<Drained>
where
    F: Future<Output = ()> + Send + 'static,
{
    let connections = Connections::new(app, Some(tls), HEADER_READ_TIMEOUT);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// What every accepted connection of one listener shares: the one router,
/// the TLS, the connection builder, the drain and the task of every
/// connection, which the drain aborts once its bound passes.
struct Connections {
    app: Router,
    tls: Option<Arc<crate::tls::ReloadableTls>>,
    builder: hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
    graceful: hyper_util::server::graceful::GracefulShutdown,
    tasks: tokio::task::JoinSet<()>,
}

impl Connections {
    fn new(
        app: Router,
        tls: Option<Arc<crate::tls::ReloadableTls>>,
        header_read_timeout: Duration,
    ) -> Self {
        let mut builder =
            hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
        // NOTE: hyper bounds the request head only with a timer set
        // (<https://docs.rs/hyper/1/hyper/server/conn/http1/struct.Builder.html#method.timer>).
        builder
            .http1()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(header_read_timeout);
        builder.http2().timer(hyper_util::rt::TokioTimer::new());
        Self {
            app,
            tls,
            builder,
            graceful: hyper_util::server::graceful::GracefulShutdown::new(),
            tasks: tokio::task::JoinSet::new(),
        }
    }

    /// Serves one accepted connection on its own task.
    fn accept(&mut self, accepted: std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>) {
        while self.tasks.try_join_next().is_some() {}
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "a connection could not be accepted");
                return;
            }
        };
        let app = self.app.clone();
        let builder = self.builder.clone();
        let watcher = self.graceful.watcher();
        let Some(tls) = &self.tls else {
            self.tasks.spawn(async move {
                let service = connection_service(app, peer, None);
                let connection = builder
                    .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service)
                    .into_owned();
                if let Err(error) = watcher.watch(connection).await {
                    tracing::debug!(error = %error, "a connection ended with an error");
                }
            });
            return;
        };
        let acceptor = tokio_rustls::TlsAcceptor::from(tls.current());
        self.tasks.spawn(async move {
            let stream =
                match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
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
            let peers = stream.get_ref().1.peer_certificates().map(|chain| {
                crate::tls::PeerCertificates(
                    chain
                        .iter()
                        .map(|certificate| certificate.clone().into_owned())
                        .collect(),
                )
            });
            let service = connection_service(app, peer, peers);
            let connection = builder
                .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service)
                .into_owned();
            if let Err(error) = watcher.watch(connection).await {
                tracing::debug!(error = %error, "a TLS connection ended with an error");
            }
        });
    }
}

/// The service of one connection: the one router, with the connection's
/// address and client certificates put on each request.
fn connection_service(
    app: Router,
    peer: std::net::SocketAddr,
    peers: Option<crate::tls::PeerCertificates>,
) -> hyper_util::service::TowerToHyperService<WithConnection> {
    hyper_util::service::TowerToHyperService::new(WithConnection { app, peer, peers })
}

/// The one router of a listener, putting one connection's facts on each
/// request it serves.
// NOTE: no specification governs this: our own design; layering the router per
// connection would build its route table again for every connection.
#[derive(Clone)]
struct WithConnection {
    app: Router,
    peer: std::net::SocketAddr,
    peers: Option<crate::tls::PeerCertificates>,
}

impl<B> tower::Service<http::Request<B>> for WithConnection
where
    Router: tower::Service<http::Request<B>>,
{
    type Response = <Router as tower::Service<http::Request<B>>>::Response;
    type Error = <Router as tower::Service<http::Request<B>>>::Error;
    type Future = <Router as tower::Service<http::Request<B>>>::Future;

    fn poll_ready(
        &mut self,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<B>>::poll_ready(&mut self.app, context)
    }

    fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
        let extensions = request.extensions_mut();
        extensions.insert(axum::extract::ConnectInfo(self.peer));
        if let Some(peers) = &self.peers {
            extensions.insert(peers.clone());
        }
        self.app.call(request)
    }
}

/// Accepts on `listener` until `signal` completes, then drains.
async fn drive<F>(
    listener: TcpListener,
    mut connections: Connections,
    lifecycle: Lifecycle,
    signal: F,
    delay: Duration,
    bound: Duration,
) -> Drained
where
    F: Future<Output = ()> + Send + 'static,
{
    lifecycle.booted();
    tokio::pin!(signal);
    loop {
        tokio::select! {
            accepted = listener.accept() => connections.accept(accepted),
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
            accepted = listener.accept() => connections.accept(accepted),
            () = &mut delayed => break,
        }
    }
    drop(listener);
    tracing::info!("the listener stops accepting; draining the requests in flight");
    let Connections {
        graceful,
        mut tasks,
        ..
    } = connections;
    let drained = if tokio::time::timeout(bound, graceful.shutdown())
        .await
        .is_ok()
    {
        Drained::Complete
    } else {
        tracing::warn!(
            shutdown_timeout_ms = bound.as_millis(),
            "the drain bound passed; the connections still open are closed"
        );
        Drained::Cut
    };
    // NOTE: `JoinSet::shutdown` aborts every task and waits for each to end
    // (<https://docs.rs/tokio/1/tokio/task/struct.JoinSet.html#method.shutdown>),
    // so no connection outlives the drain.
    tasks.shutdown().await;
    drained
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
    let section_timeout = section.header_read_timeout();
    let Some(section) = &section.tls else {
        let connections = Connections::new(app, None, section_timeout);
        return Ok(drive(listener, connections, lifecycle, signal, delay, bound).await);
    };
    let tls = Arc::new(crate::tls::ReloadableTls::new(section)?);
    let reloads = tokio::spawn(reload_on_signal(Arc::clone(&tls)));
    let interval = section.reload_interval_s.map(|seconds| {
        tokio::spawn(reload_every(
            Arc::clone(&tls),
            Duration::from_secs(seconds.get()),
        ))
    });
    let connections = Connections::new(app, Some(tls), section_timeout);
    let served = drive(listener, connections, lifecycle, signal, delay, bound).await;
    reloads.abort();
    if let Some(interval) = interval {
        interval.abort();
    }
    Ok(served)
}

/// One listener of [`run_all`]: its socket, its routes and its TLS.
#[derive(Debug)]
pub struct Listener {
    socket: TcpListener,
    app: Router,
    tls: Option<crate::section::TlsSection>,
}

impl Listener {
    /// Serves `app` on `listener` over plain HTTP.
    #[must_use]
    pub const fn new(listener: TcpListener, app: Router) -> Self {
        Self {
            socket: listener,
            app,
            tls: None,
        }
    }

    /// Serves over the TLS `tls` configures.
    #[must_use]
    pub fn with_tls(mut self, tls: crate::section::TlsSection) -> Self {
        self.tls = Some(tls);
        self
    }
}

/// Serves every listener in `listeners` until `signal` completes, then
/// drains them together, as [`run`] drains one.
///
/// Each listener keeps its own routes and TLS; `section` gives the drain
/// delay and the shutdown bound, which every listener spends at the same
/// time. A listener that ends on its own, as on an error, starts the drain
/// of the others.
///
/// # Errors
///
/// Returns the first error of a listener, as [`run`] does, and
/// [`ServeError::Listener`] when a listener's task panicked.
pub async fn run_all<F>(
    listeners: Vec<Listener>,
    section: &crate::section::ServerSection,
    lifecycle: Lifecycle,
    signal: F,
) -> Result<Drained, ServeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    for one in listeners {
        let section = crate::section::ServerSection {
            tls: one.tls,
            ..section.clone()
        };
        let lifecycle = lifecycle.clone();
        let mut stopped = stopped.clone();
        tasks.spawn(async move {
            let signal = async move {
                let _stopped: bool = stopped.wait_for(|stop| *stop).await.is_ok();
            };
            run(one.socket, one.app, &section, lifecycle, signal).await
        });
    }
    let signalled = {
        let stop = stop.clone();
        tokio::spawn(async move {
            signal.await;
            stop.send_replace(true);
        })
    };
    let mut outcome = Ok(Drained::Complete);
    while let Some(joined) = tasks.join_next().await {
        stop.send_replace(true);
        let ended = joined
            .map_err(ServeError::Listener)
            .and_then(|served| served);
        outcome = match (outcome, ended) {
            (Err(first), _) => Err(first),
            (Ok(_), Err(error)) => Err(error),
            (Ok(Drained::Cut), Ok(_)) | (Ok(_), Ok(Drained::Cut)) => Ok(Drained::Cut),
            (Ok(Drained::Complete), Ok(Drained::Complete)) => Ok(Drained::Complete),
        };
    }
    signalled.abort();
    outcome
}

/// Reads the listener's TLS files again on every `SIGHUP`.
async fn reload_on_signal(tls: Arc<crate::tls::ReloadableTls>) {
    #[cfg(unix)]
    {
        let mut hangups = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::hangup(),
        ) {
            Ok(hangups) => hangups,
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGHUP; the TLS files are not read again on it");
                return;
            }
        };
        while hangups.recv().await.is_some() {
            reload(&tls, "SIGHUP");
        }
    }
    #[cfg(not(unix))]
    {
        let _unused: &Arc<crate::tls::ReloadableTls> = &tls;
        std::future::pending::<()>().await;
    }
}

/// Reads the listener's TLS files again every `period`.
async fn reload_every(tls: Arc<crate::tls::ReloadableTls>, period: Duration) {
    let mut ticks = tokio::time::interval(period);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        reload(&tls, "interval");
    }
}

/// Reads the TLS files again, and logs the outcome.
fn reload(tls: &crate::tls::ReloadableTls, cause: &'static str) {
    match tls.reload() {
        Ok(()) => tracing::info!(cause, "the TLS files were read again"),
        Err(error) => {
            tracing::error!(cause, %error, "the TLS files do not read; the running configuration stays");
        }
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
    /// A listener's task of [`run_all`] panicked or was cancelled.
    #[error("a listener's task ended without an outcome")]
    Listener(#[source] tokio::task::JoinError),
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
            section: crate::section::TlsSection::new(certificate_file, key_file),
        }
    }

    #[tokio::test]
    async fn a_tls_listener_serves_the_pinned_healthcheck_and_drains() {
        let certificate = certificate();
        let config = std::sync::Arc::new(
            crate::tls::ReloadableTls::new(&certificate.section).expect("the TLS files read"),
        );
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
        crate::section::TlsSection::new(certificate_file, dir.join("unused.key"))
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
        let swapped = crate::section::TlsSection::new(
            certificate.section.key_file.clone(),
            certificate.section.certificate_file.clone(),
        );
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

    #[tokio::test]
    async fn a_connection_still_open_at_the_bound_is_closed_when_run_returns() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let product = Router::new().route(
            "/hold",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "held"
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
        let section = ServerSection {
            drain_delay_ms: 0,
            shutdown_timeout_ms: 300,
            ..ServerSection::default()
        };
        let app = app(
            product,
            Health::new(Registry::default(), lifecycle.clone()),
            &section,
            StackPolicy::default(),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(async move {
            super::run(listener, app, &section, lifecycle, async move {
                let _received = stopped.await;
            })
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("a connection");
        stream
            .write_all(b"GET /hold HTTP/1.1\r\nhost: localhost\r\n\r\n")
            .await
            .expect("the request is sent");
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.send(()).expect("the server listens for the signal");
        let drained = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the drain is bounded")
            .expect("the task ends")
            .expect("the server serves");
        assert_eq!(Drained::Cut, drained);
        let mut rest = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut rest)).await;
        assert!(
            read.is_ok(),
            "the held connection is closed once run returns"
        );
        assert!(rest.is_empty(), "the held request got no answer");
    }

    /// The status line and body of `GET path` on `address`.
    async fn fetch(address: std::net::SocketAddr, path: &str) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("a connection");
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .expect("the request is written");
        let mut answer = String::new();
        let _read = stream.read_to_string(&mut answer).await;
        answer
    }

    #[tokio::test]
    async fn every_listener_drains_on_one_signal() {
        let lifecycle = Lifecycle::default();
        let health = Health::new(Registry::default(), lifecycle.clone());
        let public = Router::new().route(
            "/hold",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "held"
            }),
        );
        let section = ServerSection {
            shutdown_timeout_ms: 5_000,
            ..ServerSection::default()
        };
        let mut listeners = Vec::new();
        let mut addresses = Vec::new();
        for product in [
            public,
            Router::new().route("/metrics", get(|| async { "up 1" })),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
            addresses.push(listener.local_addr().expect("the address"));
            let app = app(product, health.clone(), &section, StackPolicy::default());
            listeners.push(super::Listener::new(listener, app));
        }
        let [public, admin] = addresses[..] else {
            panic!("two listeners")
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                super::run_all(listeners, &section, lifecycle, async move {
                    let _received = stopped.await;
                })
                .await
            }
        });
        assert!(fetch(admin, "/metrics").await.ends_with("up 1"));
        let held = tokio::spawn(fetch(public, "/hold"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.send(()).expect("the server listens");
        assert!(
            held.await.expect("the request").ends_with("held"),
            "the request in flight finishes"
        );
        let drained = served.await.expect("the task").expect("the drain");
        assert_eq!(Drained::Complete, drained);
        assert_eq!(Phase::Draining, lifecycle.phase());
        for address in [public, admin] {
            assert!(
                tokio::net::TcpStream::connect(address).await.is_err(),
                "{address} is closed"
            );
        }
    }

    #[tokio::test]
    async fn a_request_over_a_connection_names_its_client_address() {
        use crate::client_address::ClientAddress;
        let product = Router::new().route(
            "/who",
            get(|axum::Extension(ClientAddress(client)): axum::Extension<ClientAddress>| async move {
                client.to_string()
            }),
        );
        let (address, _lifecycle, _stop, _served) =
            start(product, Duration::ZERO, Duration::from_secs(1)).await;
        assert!(fetch(address, "/who").await.ends_with("127.0.0.1"));
    }

    #[tokio::test]
    async fn a_client_that_never_finishes_its_head_is_disconnected() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
        let section = ServerSection {
            header_read_timeout_ms: std::num::NonZeroU64::new(200).expect("not zero"),
            ..ServerSection::default()
        };
        let app = app(
            Router::new(),
            Health::new(Registry::default(), lifecycle.clone()),
            &section,
            StackPolicy::default(),
        );
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(async move {
            super::run(listener, app, &section, lifecycle, async move {
                let _received = stopped.await;
            })
            .await
        });
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connects");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .expect("writes half a head");
        let mut rest = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
        assert!(
            read.is_ok(),
            "the server closes the connection instead of waiting for the head"
        );
        stop.send(()).expect("the server listens for the signal");
        let _drained = served.await.expect("the task ends");
    }
}
