// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The time bounds of an HTTP/1.1 connection: each request head counts from
//! its own first byte, and a kept-alive connection with no request in flight
//! closes after the idle limit.
//!
//! hyper's own head bound restarts the moment a kept-alive connection goes
//! idle, so it doubles as the idle limit and drops a request whose head is
//! still arriving when it passes. The stream here keeps the two apart, from
//! what the connection's service reports of the requests it serves.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// The HTTP/2 connection preface (RFC 9113 §3.4), which opens every HTTP/2
/// connection the server serves.
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// The two bounds of a connection between its requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Bounds {
    /// How long a request head may take from its first byte, and the first
    /// byte of a connection from accept.
    pub(super) header_read: Duration,
    /// How long a kept-alive connection may wait for the next request.
    pub(super) idle: Duration,
}

/// What the service of one connection reports to its stream: the requests
/// in flight, the requests begun, and whether the bounds no longer apply.
#[derive(Debug, Default)]
pub(super) struct Activity {
    in_flight: AtomicUsize,
    begun: AtomicU64,
    unbounded: AtomicBool,
    reader: Mutex<Option<Waker>>,
}

impl Activity {
    /// Records a request whose response is not yet written in full; the
    /// returned guard ends it when dropped.
    pub(super) fn begin(self: &Arc<Self>) -> Busy {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.begun.fetch_add(1, Ordering::AcqRel);
        Busy(Arc::clone(self))
    }

    /// Lifts the bounds for the rest of the connection: it is HTTP/2, or
    /// HTTP/1.1 upgraded to another protocol.
    pub(super) fn unbound(&self) {
        self.unbounded.store(true, Ordering::Release);
        self.wake();
    }

    /// Wakes the connection's reader, so it sees the change.
    fn wake(&self) {
        let waker = self
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Keeps `waker` to wake when the last request in flight ends, since
    /// hyper reads again only when woken.
    fn wait(&self, waker: &Waker) {
        let mut reader = self.reader.lock().unwrap_or_else(PoisonError::into_inner);
        if !reader.as_ref().is_some_and(|kept| kept.will_wake(waker)) {
            *reader = Some(waker.clone());
        }
    }
}

/// A request in flight on one connection, ended when dropped.
#[derive(Debug)]
pub(super) struct Busy(Arc<Activity>);

impl Busy {
    /// The connection the request is in flight on.
    pub(super) fn activity(&self) -> &Activity {
        &self.0
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.wake();
        }
    }
}

pin_project_lite::pin_project! {
    /// A response body that keeps its request in flight until its last
    /// frame is sent.
    pub(super) struct Tracked {
        #[pin]
        body: axum::body::Body,
        busy: Option<Busy>,
    }
}

impl Tracked {
    /// `body`, holding `busy` until it ends.
    pub(super) const fn new(body: axum::body::Body, busy: Option<Busy>) -> Self {
        Self { body, busy }
    }
}

impl hyper::body::Body for Tracked {
    type Data = <axum::body::Body as hyper::body::Body>::Data;
    type Error = <axum::body::Body as hyper::body::Body>::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let mut this = self.project();
        let polled = this.body.as_mut().poll_frame(context);
        if matches!(polled, Poll::Ready(None)) {
            *this.busy = None;
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

/// Where a connection stands between the bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Accepted, waiting for its first byte.
    Opened,
    /// A request head began arriving; `seen` requests had begun before it.
    Head { seen: u64 },
    /// A request is in flight.
    Busy,
    /// Kept alive with no request in flight; `seen` requests had begun.
    Idle { seen: u64 },
    /// HTTP/2 or upgraded: no bound applies.
    Unbounded,
}

pin_project_lite::pin_project! {
    /// An accepted stream held to the [`Bounds`].
    ///
    /// The first byte must arrive inside the head bound from accept, since
    /// the version sniff before hyper's dispatcher reads with no deadline
    /// (<https://docs.rs/hyper-util/0.1/hyper_util/server/conn/auto/struct.Builder.html>).
    /// An HTTP/1.1 head must then arrive whole inside the head bound from its
    /// first byte, and a kept-alive connection reads end of stream once it
    /// has been idle for the idle limit, which hyper answers by closing it
    /// cleanly. HTTP/2 is left to its own framing.
    pub(super) struct Bounded<S> {
        #[pin]
        stream: S,
        activity: Arc<Activity>,
        bounds: Bounds,
        phase: Phase,
        deadline: Pin<Box<tokio::time::Sleep>>,
        preface: Option<usize>,
    }
}

impl<S> Bounded<S> {
    /// `stream`, whose first byte must arrive before `deadline`, set at
    /// accept to the head bound.
    pub(super) const fn new(
        stream: S,
        activity: Arc<Activity>,
        bounds: Bounds,
        deadline: Pin<Box<tokio::time::Sleep>>,
    ) -> Self {
        Self {
            stream,
            activity,
            bounds,
            phase: Phase::Opened,
            deadline,
            preface: Some(0),
        }
    }
}

/// Moves `phase` on from what `activity` reports, arming the idle limit
/// when the connection went idle since the last read.
fn refresh(
    phase: &mut Phase,
    activity: &Activity,
    deadline: &mut Pin<Box<tokio::time::Sleep>>,
    idle: Duration,
) {
    if *phase == Phase::Unbounded {
        return;
    }
    if activity.unbounded.load(Ordering::Acquire) {
        *phase = Phase::Unbounded;
        return;
    }
    let begun = activity.begun.load(Ordering::Acquire);
    if activity.in_flight.load(Ordering::Acquire) > 0 {
        *phase = Phase::Busy;
        return;
    }
    let went_idle = match *phase {
        Phase::Busy => true,
        Phase::Opened => begun > 0,
        Phase::Head { seen } | Phase::Idle { seen } => seen != begun,
        Phase::Unbounded => false,
    };
    if went_idle {
        *phase = Phase::Idle { seen: begun };
        deadline.as_mut().reset(tokio::time::Instant::now() + idle);
    }
}

/// Matches `read` against the rest of the HTTP/2 preface after `matched`
/// bytes: the bytes matched so far while it may still be one, `None` once
/// it is not, and the whole length once it is.
fn sniff(matched: usize, read: &[u8]) -> Option<usize> {
    let rest = PREFACE.get(matched..)?;
    let length = rest.len().min(read.len());
    (rest.get(..length)? == read.get(..length)?).then_some(matched + length)
}

impl<S: tokio::io::AsyncRead> tokio::io::AsyncRead for Bounded<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();
        refresh(this.phase, this.activity, this.deadline, this.bounds.idle);
        match *this.phase {
            Phase::Unbounded => return this.stream.poll_read(context, buffer),
            Phase::Busy => {
                let polled = this.stream.poll_read(context, buffer);
                if polled.is_pending() {
                    this.activity.wait(context.waker());
                    let unbounded = this.activity.unbounded.load(Ordering::Acquire);
                    if unbounded || this.activity.in_flight.load(Ordering::Acquire) == 0 {
                        context.waker().wake_by_ref();
                    }
                }
                return polled;
            }
            Phase::Opened | Phase::Head { .. } | Phase::Idle { .. } => {}
        }
        let before = buffer.filled().len();
        let polled = this.stream.poll_read(context, buffer);
        if polled.is_pending() {
            this.activity.wait(context.waker());
        }
        match &polled {
            Poll::Ready(Ok(())) if buffer.filled().len() > before => {
                if let Some(matched) = *this.preface {
                    let read = buffer.filled().get(before..).unwrap_or_default();
                    *this.preface = sniff(matched, read);
                    if *this.preface == Some(PREFACE.len()) {
                        *this.phase = Phase::Unbounded;
                        return polled;
                    }
                }
                let seen = match *this.phase {
                    Phase::Opened => Some(0),
                    Phase::Idle { seen } => Some(seen),
                    Phase::Head { .. } | Phase::Busy | Phase::Unbounded => None,
                };
                if let Some(seen) = seen {
                    *this.phase = Phase::Head { seen };
                    this.deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + this.bounds.header_read);
                }
            }
            Poll::Pending if this.deadline.as_mut().poll(context).is_ready() => {
                // NOTE: RFC 9112 §9.5 lets a server close an idle connection; end of
                // stream while idle makes hyper close it cleanly with no error.
                if matches!(*this.phase, Phase::Idle { .. }) {
                    return Poll::Ready(Ok(()));
                }
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the client did not send a request head inside the header read timeout",
                )));
            }
            Poll::Ready(_) | Poll::Pending => {}
        }
        polled
    }
}

impl<S: tokio::io::AsyncWrite> tokio::io::AsyncWrite for Bounded<S> {
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::time::Duration;

    use ferrohealth_health::lifecycle::Lifecycle;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    use super::{PREFACE, sniff};
    use crate::section::ServerSection;

    #[test]
    fn the_preface_is_recognised_across_reads_and_a_request_line_is_not() {
        assert_eq!(Some(3), sniff(0, b"PRI"));
        assert_eq!(Some(PREFACE.len()), sniff(3, &PREFACE[3..]));
        assert_eq!(Some(PREFACE.len()), sniff(0, PREFACE));
        assert_eq!(None, sniff(0, b"GET / HTTP/1.1\r\n"));
        assert_eq!(None, sniff(3, b"X"));
    }

    /// Serves a route answering `OK` under `section`, and returns its port.
    async fn start(section: ServerSection) -> u16 {
        let app = axum::Router::new().route("/ok", axum::routing::get(|| async { "OK" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let port = listener.local_addr().expect("the address").port();
        tokio::spawn(async move {
            let _served = crate::serve::run(
                listener,
                app,
                &section,
                Lifecycle::default(),
                std::future::pending(),
            )
            .await;
        });
        port
    }

    fn bounds(header_read_ms: u64, idle_ms: u64) -> ServerSection {
        ServerSection {
            header_read_timeout_ms: NonZeroU64::new(header_read_ms).expect("non-zero"),
            idle_timeout_ms: NonZeroU64::new(idle_ms).expect("non-zero"),
            ..ServerSection::default()
        }
    }

    /// Connects and reads the answer to a first `GET /ok`, kept alive.
    async fn answered_once(port: u16) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connects");
        socket
            .write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("writes the first request");
        let mut first = [0_u8; 256];
        let read = socket.read(&mut first).await.expect("reads the answer");
        let first = String::from_utf8_lossy(first.get(..read).unwrap_or_default()).to_string();
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        socket
    }

    /// Everything the server sends until it closes, waiting at most five seconds.
    async fn rest(socket: &mut TcpStream) -> String {
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut answer))
            .await
            .expect("the server answers or closes inside five seconds")
            .expect("the read ends cleanly");
        String::from_utf8_lossy(&answer).to_string()
    }

    /// Writes a second `GET /ok` in two parts, `pause` apart.
    async fn second_request_in_two_parts(socket: &mut TcpStream, pause: Duration) {
        socket
            .write_all(b"GET /ok HTTP/1.1\r\n")
            .await
            .expect("writes the request line");
        tokio::time::sleep(pause).await;
        socket
            .write_all(b"Host: localhost\r\nConnection: close\r\n\r\n")
            .await
            .expect("writes the rest of the head");
    }

    #[tokio::test]
    async fn a_request_begun_on_a_kept_alive_connection_is_answered() {
        let port = start(bounds(1_000, 120_000)).await;
        let mut socket = answered_once(port).await;
        tokio::time::sleep(Duration::from_millis(900)).await;
        second_request_in_two_parts(&mut socket, Duration::from_millis(300)).await;
        let answer = rest(&mut socket).await;
        assert!(
            answer.starts_with("HTTP/1.1 200"),
            "the head bound counts from the head's first byte, not from the previous response: {answer:?}"
        );
    }

    #[tokio::test]
    async fn a_head_that_takes_longer_than_the_bound_from_its_first_byte_is_refused() {
        let port = start(bounds(500, 120_000)).await;
        let mut socket = answered_once(port).await;
        socket
            .write_all(b"GET /ok HTTP/1.1\r\n")
            .await
            .expect("writes the request line");
        let started = std::time::Instant::now();
        let answer = rest(&mut socket).await;
        assert!(
            !answer.starts_with("HTTP/1.1 200"),
            "an unfinished head is not answered: {answer:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the head bound, not the idle limit, closed the connection: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_request_begun_just_before_the_idle_limit_is_answered() {
        let port = start(bounds(1_000, 1_000)).await;
        let mut socket = answered_once(port).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        second_request_in_two_parts(&mut socket, Duration::from_millis(400)).await;
        let answer = rest(&mut socket).await;
        assert!(
            answer.starts_with("HTTP/1.1 200"),
            "a head begun inside the idle limit is answered though it ends past it: {answer:?}"
        );
    }

    #[tokio::test]
    async fn a_connection_idle_past_the_limit_is_closed_without_an_answer() {
        let port = start(bounds(30_000, 600)).await;
        let mut socket = answered_once(port).await;
        let started = std::time::Instant::now();
        let answer = rest(&mut socket).await;
        let closed_after = started.elapsed();
        assert_eq!("", answer, "an idle connection closes with nothing sent");
        assert!(
            closed_after >= Duration::from_millis(500) && closed_after < Duration::from_secs(3),
            "the idle limit, not the 30 s head bound, closed it: {closed_after:?}"
        );
    }
}
