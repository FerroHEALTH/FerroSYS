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
//! timeout, counted from accept, is closed. An `accept` error that is not one
//! connection's, such as `EMFILE`, pauses the listener from 5 ms doubling up
//! to a second, and is logged at most once every ten seconds.
//!
//! An HTTP/1.1 response whose request body the routes left unread says
//! `Connection: close`, because the connection closes after it (RFC 9112
//! §9.6), so a client opens a new one for its next request.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
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
    let connections = Connections::new(app, None, HEADER_READ_TIMEOUT);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// How long a client may take to finish the TLS handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a client may take to send a request head when the caller names
/// no bound.
///
/// The bound starts when the connection is accepted: a connection whose first
/// byte does not arrive inside it is closed, over plain HTTP and over TLS, for
/// HTTP/1.1 and HTTP/2 alike.
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
    let connections = Connections::new(app, Some(tls), HEADER_READ_TIMEOUT);
    Ok(drive(listener, connections, lifecycle, signal, delay, bound).await)
}

/// What every accepted connection of one listener shares: the one router,
/// the TLS, the connection builder, the drain, the task of every connection
/// and the task of every HTTP/2 stream, which the drain aborts once its
/// bound passes.
struct Connections {
    app: Router,
    tls: Option<Arc<crate::tls::ReloadableTls>>,
    header_read_timeout: Duration,
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

pin_project_lite::pin_project! {
    /// An accepted stream whose first byte must arrive before a deadline set
    /// at accept.
    ///
    /// hyper's HTTP/1 header bound starts only once its dispatcher reads a
    /// head, and the version sniff before it reads with no deadline
    /// (<https://docs.rs/hyper-util/0.1/hyper_util/server/conn/auto/struct.Builder.html>),
    /// so a connection that never sends a byte would be held for ever.
    struct FirstByte<S> {
        #[pin]
        stream: S,
        deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    }
}

impl<S> FirstByte<S> {
    const fn new(stream: S, deadline: Pin<Box<tokio::time::Sleep>>) -> Self {
        Self {
            stream,
            deadline: Some(deadline),
        }
    }
}

impl<S: tokio::io::AsyncRead> tokio::io::AsyncRead for FirstByte<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();
        let before = buffer.filled().len();
        let polled = this.stream.poll_read(context, buffer);
        match &polled {
            Poll::Ready(Ok(())) if buffer.filled().len() > before => *this.deadline = None,
            Poll::Pending => {
                if let Some(deadline) = this.deadline.as_mut()
                    && deadline.as_mut().poll(context).is_ready()
                {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the client sent no byte inside the header read timeout",
                    )));
                }
            }
            Poll::Ready(_) => {}
        }
        polled
    }
}

impl<S: tokio::io::AsyncWrite> tokio::io::AsyncWrite for FirstByte<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().stream.poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().stream.poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().stream.poll_shutdown(context)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        self.project().stream.poll_write_vectored(context, buffers)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
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
        header_read_timeout: Duration,
    ) -> Self {
        let streams = Streams::default();
        let mut builder = hyper_util::server::conn::auto::Builder::new(streams.clone());
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
            header_read_timeout,
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
        let first_byte = Box::pin(tokio::time::sleep(self.header_read_timeout));
        let app = self.app.clone();
        let builder = self.builder.clone();
        let watcher = self.graceful.watcher();
        let Some(tls) = &self.tls else {
            self.tasks.spawn(async move {
                let service = connection_service(app, peer, None);
                let stream = FirstByte::new(stream, first_byte);
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
            let stream = FirstByte::new(stream, first_byte);
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
    B: hyper::body::Body,
    Router: tower::Service<http::Request<ReadToEnd<B>>, Response = axum::response::Response>,
{
    type Response = axum::response::Response;
    type Error = <Router as tower::Service<http::Request<ReadToEnd<B>>>>::Error;
    type Future = CloseUnread<<Router as tower::Service<http::Request<ReadToEnd<B>>>>::Future>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<ReadToEnd<B>>>::poll_ready(&mut self.app, context)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let http1 = request.version() < http::Version::HTTP_2;
        let (mut parts, body) = request.into_parts();
        parts
            .extensions
            .insert(axum::extract::ConnectInfo(self.peer));
        if let Some(peers) = &self.peers {
            parts.extensions.insert(peers.clone());
        }
        let read = Arc::new(AtomicBool::new(body.is_end_stream()));
        let body = ReadToEnd {
            body,
            read: Arc::clone(&read),
        };
        CloseUnread {
            response: self.app.call(http::Request::from_parts(parts, body)),
            read: http1.then_some(read),
        }
    }
}

pin_project_lite::pin_project! {
    /// A request body that records whether it was read to its end.
    struct ReadToEnd<B> {
        #[pin]
        body: B,
        read: Arc<AtomicBool>,
    }
}

impl<B: hyper::body::Body> hyper::body::Body for ReadToEnd<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        let polled = this.body.as_mut().poll_frame(context);
        if matches!(polled, Poll::Ready(None)) || this.body.is_end_stream() {
            this.read.store(true, Ordering::Release);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

/// Whether `headers` carry the "close" connection option (RFC 9112 §9.6).
fn says_close(headers: &http::HeaderMap) -> bool {
    headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|options| options.split(','))
        .any(|option| option.trim().eq_ignore_ascii_case("close"))
}

pin_project_lite::pin_project! {
    /// The response to an HTTP/1 request, which says `Connection: close` when
    /// the request body was left unread.
    struct CloseUnread<F> {
        #[pin]
        response: F,
        read: Option<Arc<AtomicBool>>,
    }
}

impl<F, E> Future for CloseUnread<F>
where
    F: Future<Output = Result<axum::response::Response, E>>,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut response = std::task::ready!(this.response.poll(context))?;
        let unread = this
            .read
            .as_ref()
            .is_some_and(|read| !read.load(Ordering::Acquire));
        // NOTE: RFC 9112 §9.6: a sender SHOULD send the "close" option when it
        // intends to close, as hyper does after a response that left a body unread.
        if unread && !says_close(response.headers()) {
            response.headers_mut().append(
                http::header::CONNECTION,
                http::HeaderValue::from_static("close"),
            );
        }
        Poll::Ready(Ok(response))
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

    #[tokio::test]
    async fn an_accepted_connection_sends_without_delay() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let _client = tokio::net::TcpStream::connect(address)
            .await
            .expect("a connection");
        let (accepted, _peer) = listener.accept().await.expect("accepted");
        assert!(!accepted.nodelay().expect("the option reads"));
        super::no_delay(&accepted);
        assert!(
            accepted.nodelay().expect("the option reads"),
            "every accepted connection, plain or TLS, has TCP_NODELAY set"
        );
    }

    /// A handler that never answers and, once woken, holds its worker for
    /// half a second in each poll, so only an abort the drain waits for
    /// ends it before `run` returns.
    struct Stubborn {
        polls: usize,
        started: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        _running: Running,
    }

    impl Future for Stubborn {
        type Output = &'static str;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            self.polls += 1;
            if self.polls == 1 {
                self.started
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            } else {
                std::thread::sleep(Duration::from_millis(500));
            }
            std::task::Poll::Pending
        }
    }

    /// Counts its own drop, so a test sees a handler cancelled.
    struct Running(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for Running {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_http2_handler_still_running_at_the_bound_is_cancelled_when_run_returns() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let started = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let product = Router::new().route(
            "/hold",
            get({
                let (started, dropped) = (started.clone(), dropped.clone());
                move || Stubborn {
                    polls: 0,
                    started,
                    _running: Running(dropped),
                }
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
        let stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("a connection");
        let (mut sender, connection) = hyper::client::conn::http2::handshake(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(stream),
        )
        .await
        .expect("the HTTP/2 handshake");
        let client = tokio::spawn(connection);
        let request = tokio::spawn(
            sender.send_request(
                http::Request::get("http://localhost/hold")
                    .body(axum::body::Body::empty())
                    .expect("the request builds"),
            ),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while started.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the handler starts");
        stop.send(()).expect("the server listens for the signal");
        let drained = tokio::time::timeout(Duration::from_secs(5), served)
            .await
            .expect("the drain is bounded")
            .expect("the task ends")
            .expect("the server serves");
        assert_eq!(Drained::Cut, drained);
        assert_eq!(
            1,
            dropped.load(Ordering::SeqCst),
            "the handler is cancelled before run returns"
        );
        request.abort();
        client.abort();
    }

    /// Serves `product` through the stack with `section` on a fresh port
    /// until the returned sender fires.
    async fn start_run(
        product: Router,
        section: ServerSection,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<Drained, super::ServeError>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
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
        (address, stop, served)
    }

    fn short_head_bound() -> ServerSection {
        ServerSection {
            header_read_timeout_ms: std::num::NonZeroU64::new(200).expect("not zero"),
            drain_delay_ms: 0,
            ..ServerSection::default()
        }
    }

    /// How long until `stream` is closed by the server, waiting at most five
    /// seconds; `None` when it is still open then.
    async fn closed_after<S>(stream: &mut S) -> Option<Duration>
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        use tokio::io::AsyncReadExt as _;
        let started = std::time::Instant::now();
        let mut rest = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
        read.is_ok().then(|| started.elapsed())
    }

    #[tokio::test]
    async fn a_connection_that_sends_no_byte_is_closed_at_the_header_read_timeout() {
        let (address, stop, served) = start_run(Router::new(), short_head_bound()).await;
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connects");
        let closed = closed_after(&mut stream).await;
        assert!(
            closed.is_some_and(|after| after >= Duration::from_millis(150)),
            "a silent connection is closed once the bound from accept passes: {closed:?}"
        );
        assert_eq!(
            Outcome::Healthy,
            check(address, "", Duration::from_secs(2)).await,
            "the listener still serves"
        );
        stop.send(()).expect("the server listens for the signal");
        let _drained = served.await.expect("the task ends");
    }

    #[tokio::test]
    async fn a_tls_connection_that_sends_no_request_byte_is_closed_at_the_header_read_timeout() {
        let certificate = certificate();
        let section = ServerSection {
            tls: Some(certificate.section.clone()),
            ..short_head_bound()
        };
        let (address, stop, served) = start_run(Router::new(), section).await;
        let pinned = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
        let stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connects");
        let mut stream = tokio_rustls::TlsConnector::from(pinned.config())
            .connect(
                rustls::pki_types::ServerName::IpAddress(address.ip().into()),
                stream,
            )
            .await
            .expect("the handshake completes");
        assert!(
            closed_after(&mut stream).await.is_some(),
            "a connection silent after its handshake is closed once the bound passes"
        );
        stop.send(()).expect("the server listens for the signal");
        let _drained = served.await.expect("the task ends");
    }

    /// Sends a `POST path` with `size` body bytes, then `GET /health/liveness`
    /// on the same connection, and returns everything the server answered.
    async fn post_then_get(address: std::net::SocketAddr, path: &str, size: usize) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connects");
        let (mut reader, mut writer) = stream.into_split();
        let head = format!(
            "POST {path} HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/xml\r\ncontent-length: {size}\r\n\r\n"
        );
        let sent = tokio::spawn(async move {
            writer.write_all(head.as_bytes()).await?;
            writer.write_all(&vec![b'x'; size]).await?;
            writer
                .write_all(b"GET /health/liveness HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
                .await?;
            Ok::<_, std::io::Error>(writer)
        });
        let mut answer = Vec::new();
        let _read = tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut answer))
            .await
            .expect("the server answers or closes");
        let _sent = sent.await;
        String::from_utf8_lossy(&answer).to_ascii_lowercase()
    }

    #[tokio::test]
    async fn a_large_body_left_unread_closes_the_connection_with_connection_close() {
        let (address, stop, served) = start_run(Router::new(), short_head_bound()).await;
        let answer = post_then_get(address, "/definition/template/t/validation", 300_000).await;
        let first = answer.split("\r\n\r\n").next().unwrap_or_default();
        assert!(first.starts_with("http/1.1 404"), "{answer}");
        let second_answered = answer.matches("http/1.1 ").count() > 1;
        assert!(
            second_answered || first.contains("\r\nconnection: close"),
            "the second request is answered or the first response says it closes: {answer}"
        );
        stop.send(()).expect("the server listens for the signal");
        let _drained = served.await.expect("the task ends");
    }

    #[tokio::test]
    async fn a_body_read_to_its_end_keeps_the_connection() {
        let product = Router::new().route(
            "/echo",
            axum::routing::post(|body: axum::body::Bytes| async move { body.len().to_string() }),
        );
        let (address, stop, served) = start_run(product, short_head_bound()).await;
        let answer = post_then_get(address, "/echo", 300_000).await;
        let first = answer.split("\r\n\r\n").next().unwrap_or_default();
        assert!(first.starts_with("http/1.1 200"), "{answer}");
        assert!(!first.contains("connection: close"), "{answer}");
        assert_eq!(
            2,
            answer.matches("http/1.1 200").count(),
            "the connection serves the second request: {answer}"
        );
        stop.send(()).expect("the server listens for the signal");
        let _drained = served.await.expect("the task ends");
    }

    #[test]
    fn an_accept_error_pauses_more_each_time_and_is_logged_once_a_window() {
        let mut errors = super::AcceptErrors::default();
        let now = tokio::time::Instant::now();
        let emfile = std::io::Error::from_raw_os_error(24);
        let pauses: Vec<_> = (0..10).map(|_| errors.failed(&emfile, now)).collect();
        assert_eq!(super::ACCEPT_PAUSE_FIRST, pauses[0]);
        assert_eq!(super::ACCEPT_PAUSE_FIRST * 2, pauses[1]);
        assert_eq!(super::ACCEPT_PAUSE_MOST, pauses[9], "{pauses:?}");
        assert_eq!(9, errors.suppressed, "one warning in the window");
        let later = now + super::ACCEPT_LOG_EVERY;
        let _pause = errors.failed(&emfile, later);
        assert_eq!(0, errors.suppressed, "the next window warns again");
        errors.accepted();
        assert_eq!(super::ACCEPT_PAUSE_FIRST, errors.failed(&emfile, later));
        let reset = std::io::Error::from(std::io::ErrorKind::ConnectionAborted);
        assert_eq!(Duration::ZERO, errors.failed(&reset, later));
    }

    /// A listener whose every `accept` fails with `EMFILE`, counting the
    /// attempts; after `limit` of them it never answers again.
    struct Exhausted {
        attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        limit: usize,
    }

    impl super::Accept for Exhausted {
        async fn accept(
            &mut self,
        ) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
            let attempts = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if attempts >= self.limit {
                std::future::pending::<()>().await;
            }
            Err(std::io::Error::from_raw_os_error(24))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_accept_loop_backs_off_while_descriptors_are_exhausted() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = Exhausted {
            attempts: attempts.clone(),
            limit: 10_000,
        };
        let connections = super::Connections::new(Router::new(), None, super::HEADER_READ_TIMEOUT);
        let drained = super::drive(
            listener,
            connections,
            Lifecycle::default(),
            tokio::time::sleep(Duration::from_secs(3)),
            Duration::ZERO,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(Drained::Complete, drained);
        let attempts = attempts.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (2..=12).contains(&attempts),
            "three seconds of EMFILE take a few attempts, not a spin: {attempts}"
        );
    }
}
