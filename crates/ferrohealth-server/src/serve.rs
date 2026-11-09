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
//!
//! A connection whose first byte does not arrive inside the header read
//! timeout, counted from accept, is closed, and so is an HTTP/1.1 connection
//! whose request head does not arrive whole inside it, counted from the
//! head's first byte. A kept-alive HTTP/1.1 connection with no request in
//! flight is closed after the idle timeout, which counts apart from the head
//! bound, so a request begun on a reused connection is answered. An `accept` error that is not one
//! connection's, such as `EMFILE`, pauses the listener from 5 ms doubling up
//! to a second, and is logged at most once every ten seconds.
//!
//! An HTTP/1.1 response whose request body the routes left unread says
//! `Connection: close`, because the connection closes after it (RFC 9112
//! §9.6), so a client opens a new one for its next request.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use ferrohealth_health::lifecycle::Lifecycle;
use tokio::net::TcpListener;

mod connection;
mod keep_alive;

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
/// [`HEADER_READ_TIMEOUT`] to send a request head and a kept-alive
/// connection is closed after [`IDLE_TIMEOUT`]; [`run`] takes both bounds
/// from the section.
///
/// # Errors
///
/// Never fails today: an accept error is logged and the listener goes on,
/// pausing first when the error is not one connection's.
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
    let connections = Connections::new(app, None, DEFAULT_BOUNDS);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// How long a client may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send a request head when the caller names
/// no bound.
///
/// A connection whose first byte does not arrive inside it from accept is
/// closed, over plain HTTP and over TLS, for HTTP/1.1 and HTTP/2 alike. An
/// HTTP/1.1 request head must then arrive whole inside it from its own first
/// byte; the time a kept-alive connection was idle before the head does not
/// count.
///
/// It is hyper's own default for HTTP/1
/// (<https://docs.rs/hyper/1/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout>).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a kept-alive HTTP/1.1 connection may wait for its next request
/// when the caller names no bound.
///
/// It is longer than the idle timeouts clients and proxies keep by default,
/// such as the 90 s of Go's `http.DefaultTransport`, so a client normally
/// closes an idle connection before the server does and never writes a
/// request into one the server is closing (RFC 9112 §9.5).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The bounds of [`serve`] and [`serve_tls`].
const DEFAULT_BOUNDS: keep_alive::Bounds = keep_alive::Bounds {
    header_read: HEADER_READ_TIMEOUT,
    idle: IDLE_TIMEOUT,
};

/// Serves `app` over TLS on `listener` until `signal` completes, then
/// drains, as [`serve`] does over plain HTTP.
///
/// Each connection's TLS handshake is bounded by [`HANDSHAKE_TIMEOUT`]; a
/// failed handshake drops that connection alone. HTTP/2 and HTTP/1.1 are
/// served as the client chose by ALPN.
///
/// # Errors
///
/// Never fails today: an accept error is logged and the listener goes on,
/// pausing first when the error is not one connection's.
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
    let connections = Connections::new(app, Some(tls), DEFAULT_BOUNDS);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// What every accepted connection of one listener shares: the one router,
/// the TLS, the connection builder, the drain, the task of every connection
/// and the task of every HTTP/2 stream, which the drain aborts once its
/// bound passes.
struct Connections {
    app: Router,
    tls: Option<Arc<crate::tls::ReloadableTls>>,
    bounds: keep_alive::Bounds,
    builder: hyper_util::server::conn::auto::Builder<Streams>,
    graceful: hyper_util::server::graceful::GracefulShutdown,
    tasks: tokio::task::JoinSet<()>,
    streams: Streams,
}

/// The executor of the HTTP/2 streams of one listener.
///
/// hyper serves each HTTP/2 request on a task its executor spawns, apart
/// from the connection's task
/// (<https://docs.rs/hyper/1/hyper/server/conn/http2/struct.Builder.html#method.new>),
/// so aborting the connection would leave the handler running; these tasks
/// live in one set the drain aborts with the connections.
#[derive(Clone, Default)]
struct Streams(Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>);

impl<F> hyper::rt::Executor<F> for Streams
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, future: F) {
        let mut tasks = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while tasks.try_join_next().is_some() {}
        tasks.spawn(future);
    }
}

impl Streams {
    /// Aborts every stream still running and waits for each to end.
    async fn shutdown(&self) {
        let mut tasks = {
            let mut guard = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *guard)
        };
        tasks.shutdown().await;
    }
}

/// Sets `TCP_NODELAY` on an accepted connection, so a small response is
/// sent at once.
fn no_delay(stream: &tokio::net::TcpStream) {
    // NOTE: RFC 9293 §3.7.4 lets a sender hold a small segment back while data
    // is unacknowledged, which the peer's delayed ACK stretches per response.
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(%error, "TCP_NODELAY could not be set on a connection");
    }
}

/// The first pause after an `accept` error that is not one connection's.
const ACCEPT_PAUSE_FIRST: Duration = Duration::from_millis(5);

/// The longest pause after an `accept` error, which axum's own `serve` waits
/// after each one (<https://docs.rs/axum/0.8/axum/fn.serve.html>).
const ACCEPT_PAUSE_MOST: Duration = Duration::from_secs(1);

/// How often an `accept` error is logged at most.
const ACCEPT_LOG_EVERY: Duration = Duration::from_secs(10);

/// The `accept` errors of one listener: the pause before the next `accept`
/// and the time of the last warning.
///
/// An error such as `EMFILE` or `ENFILE` lasts until a descriptor frees, so
/// accepting again at once would spin and flood the log.
#[derive(Debug, Default)]
struct AcceptErrors {
    pause: Duration,
    logged_at: Option<tokio::time::Instant>,
    suppressed: u64,
}

impl AcceptErrors {
    /// Records `error` at `now` and returns the pause before the next
    /// `accept`: none for an error of the one connection, and a pause that
    /// doubles up to [`ACCEPT_PAUSE_MOST`] for the others.
    fn failed(&mut self, error: &std::io::Error, now: tokio::time::Instant) -> Duration {
        // NOTE: no specification governs this: our own design; axum's `serve`
        // also goes on at once on these three kinds, which end one connection.
        let one_connection = matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
        );
        if !one_connection {
            self.pause = (self.pause * 2).clamp(ACCEPT_PAUSE_FIRST, ACCEPT_PAUSE_MOST);
        }
        if self
            .logged_at
            .is_some_and(|logged| now.saturating_duration_since(logged) < ACCEPT_LOG_EVERY)
        {
            self.suppressed = self.suppressed.saturating_add(1);
        } else {
            let pause_ms = if one_connection {
                0
            } else {
                self.pause.as_millis()
            };
            tracing::warn!(
                %error,
                pause_ms,
                suppressed = self.suppressed,
                "a connection could not be accepted"
            );
            self.logged_at = Some(now);
            self.suppressed = 0;
        }
        if one_connection {
            Duration::ZERO
        } else {
            self.pause
        }
    }

    /// Records an accepted connection, which ends the pauses.
    const fn accepted(&mut self) {
        self.pause = Duration::ZERO;
    }
}

/// Where a listener's connections come from: the socket, or a test's stand-in.
trait Accept {
    /// The next connection, or the error of the attempt.
    fn accept(
        &mut self,
    ) -> impl Future<Output = std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)>> + Send;
}

impl Accept for TcpListener {
    async fn accept(&mut self) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
        TcpListener::accept(self).await
    }
}

impl Connections {
    fn new(
        app: Router,
        tls: Option<Arc<crate::tls::ReloadableTls>>,
        bounds: keep_alive::Bounds,
    ) -> Self {
        let streams = Streams::default();
        let mut builder = hyper_util::server::conn::auto::Builder::new(streams.clone());
        // NOTE: hyper 1.12 restarts its head bound when a kept-alive connection goes
        // idle (`proto/h1/conn.rs`, `State::idle`), so `keep_alive::Bounded` holds both bounds.
        builder
            .http1()
            .timer(hyper_util::rt::TokioTimer::new())
            .header_read_timeout(None);
        builder.http2().timer(hyper_util::rt::TokioTimer::new());
        Self {
            app,
            tls,
            bounds,
            builder,
            graceful: hyper_util::server::graceful::GracefulShutdown::new(),
            tasks: tokio::task::JoinSet::new(),
            streams,
        }
    }

    /// Serves one accepted connection on its own task.
    fn accept(&mut self, stream: tokio::net::TcpStream, peer: std::net::SocketAddr) {
        while self.tasks.try_join_next().is_some() {}
        no_delay(&stream);
        // NOTE: RFC 9112 §9.5 lets a server close a connection it holds inactive;
        // the bound counts from accept so the version sniff is held to it too.
        let accepted = connection::Accepted {
            app: self.app.clone(),
            peer,
            builder: self.builder.clone(),
            watcher: self.graceful.watcher(),
            bounds: self.bounds,
            first_byte: Box::pin(tokio::time::sleep(self.bounds.header_read)),
        };
        let Some(tls) = &self.tls else {
            self.tasks
                .spawn(accepted.serve(stream, None, "a connection ended with an error"));
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
            accepted
                .serve(stream, peers, "a TLS connection ended with an error")
                .await;
        });
    }
}
/// Accepts on `listener` until `stop` completes, pausing after an `accept`
/// error as `errors` says; returns once `stop` completed.
async fn accept_until<L, S>(
    listener: &mut L,
    connections: &mut Connections,
    errors: &mut AcceptErrors,
    stop: Pin<&mut S>,
) where
    L: Accept,
    S: Future<Output = ()>,
{
    let mut stop = stop;
    loop {
        let pause = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    errors.accepted();
                    connections.accept(stream, peer);
                    continue;
                }
                Err(error) => errors.failed(&error, tokio::time::Instant::now()),
            },
            () = stop.as_mut() => return,
        };
        if pause.is_zero() {
            continue;
        }
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            () = stop.as_mut() => return,
        }
    }
}

/// Accepts on `listener` until `signal` completes, then drains.
async fn drive<L, F>(
    mut listener: L,
    mut connections: Connections,
    lifecycle: Lifecycle,
    signal: F,
    delay: Duration,
    bound: Duration,
) -> Drained
where
    L: Accept,
    F: Future<Output = ()> + Send + 'static,
{
    lifecycle.booted();
    let mut errors = AcceptErrors::default();
    tokio::pin!(signal);
    accept_until(
        &mut listener,
        &mut connections,
        &mut errors,
        signal.as_mut(),
    )
    .await;
    lifecycle.drain();
    tracing::info!(
        drain_delay_ms = delay.as_millis(),
        "readiness withdrawn; the listener accepts until the drain delay ends"
    );
    let delayed = tokio::time::sleep(delay);
    tokio::pin!(delayed);
    accept_until(
        &mut listener,
        &mut connections,
        &mut errors,
        delayed.as_mut(),
    )
    .await;
    drop(listener);
    tracing::info!("the listener stops accepting; draining the requests in flight");
    let Connections {
        graceful,
        mut tasks,
        streams,
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
    // so no connection, and then no HTTP/2 stream, outlives the drain.
    tasks.shutdown().await;
    streams.shutdown().await;
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
    let tls = match &section.tls {
        Some(tls) => Some(Arc::new(crate::tls::ReloadableTls::new(tls)?)),
        None => None,
    };
    run_with_tls(listener, app, section, tls, lifecycle, signal).await
}

/// Serves `app` as [`run`] does, over `tls` when it is set, which the
/// product built and keeps; `section.tls` is not read.
///
/// The server reads `tls` again on `SIGHUP` and every `reload_interval`
/// of its table, and the product may call [`crate::tls::ReloadableTls::reload`]
/// from its own reload path too.
///
/// # Errors
///
/// Returns [`ServeError::Io`] for an I/O error of the listener.
pub async fn run_with_tls<F>(
    listener: TcpListener,
    app: Router,
    section: &crate::section::ServerSection,
    tls: Option<Arc<crate::tls::ReloadableTls>>,
    lifecycle: Lifecycle,
    signal: F,
) -> Result<Drained, ServeError>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (delay, bound) = (section.drain_delay.get(), section.shutdown_timeout.get());
    let bounds = keep_alive::Bounds {
        header_read: section.header_read_timeout.get(),
        idle: section.idle_timeout.get(),
    };
    let Some(tls) = tls else {
        let connections = Connections::new(app, None, bounds);
        return Ok(drive(listener, connections, lifecycle, signal, delay, bound).await);
    };
    let section = tls.section().clone();
    let reloads = tokio::spawn(reload_on_signal(Arc::clone(&tls)));
    let interval = section
        .reload_interval
        .map(|every| tokio::spawn(reload_every(Arc::clone(&tls), every.get())));
    let connections = Connections::new(app, Some(tls), bounds);
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
    tls: Option<ListenerTls>,
}

/// The TLS of one listener: a table the server reads, or a configuration the
/// product keeps.
#[derive(Debug)]
enum ListenerTls {
    Section(crate::section::TlsSection),
    Kept(Arc<crate::tls::ReloadableTls>),
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
        self.tls = Some(ListenerTls::Section(tls));
        self
    }

    /// Serves over `tls`, which the product built and keeps, so it can read
    /// it again from its own reload path and its faults name the keys it
    /// was built with ([`crate::tls::ReloadableTls::of`]).
    #[must_use]
    pub fn with_reloadable_tls(mut self, tls: Arc<crate::tls::ReloadableTls>) -> Self {
        self.tls = Some(ListenerTls::Kept(tls));
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
        let tls = match one.tls {
            Some(ListenerTls::Section(tls)) => {
                Some(Arc::new(crate::tls::ReloadableTls::new(&tls)?))
            }
            Some(ListenerTls::Kept(tls)) => Some(tls),
            None => None,
        };
        let section = crate::section::ServerSection {
            tls: None,
            ..section.clone()
        };
        let lifecycle = lifecycle.clone();
        let mut stopped = stopped.clone();
        tasks.spawn(async move {
            let signal = async move {
                let _stopped: bool = stopped.wait_for(|stop| *stop).await.is_ok();
            };
            run_with_tls(one.socket, one.app, &section, tls, lifecycle, signal).await
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
mod tests;
