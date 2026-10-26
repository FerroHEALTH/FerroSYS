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
