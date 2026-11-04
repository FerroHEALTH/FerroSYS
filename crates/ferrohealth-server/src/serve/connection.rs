// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One accepted connection: serving it once its transport is known, and the
//! service that puts the connection's facts on each request it carries.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use axum::Router;

use super::Streams;
use super::keep_alive;

/// One accepted connection, with what serving it needs once its transport is
/// known: the plain socket, or the stream of a finished TLS handshake.
pub(super) struct Accepted {
    pub(super) app: Router,
    pub(super) peer: std::net::SocketAddr,
    pub(super) builder: hyper_util::server::conn::auto::Builder<Streams>,
    pub(super) watcher: hyper_util::server::graceful::Watcher,
    pub(super) bounds: keep_alive::Bounds,
    pub(super) first_byte: Pin<Box<tokio::time::Sleep>>,
}

impl Accepted {
    /// Serves the connection on `stream`, held to the bounds from accept,
    /// until it ends or the drain closes it; `ended` is the debug line of a
    /// connection that ends with an error.
    pub(super) async fn serve<S>(
        self,
        stream: S,
        peers: Option<crate::tls::PeerCertificates>,
        ended: &'static str,
    ) where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let activity = Arc::new(keep_alive::Activity::default());
        let service = connection_service(self.app, self.peer, peers, Arc::clone(&activity));
        let stream = keep_alive::Bounded::new(stream, activity, self.bounds, self.first_byte);
        let connection = self
            .builder
            .serve_connection_with_upgrades(hyper_util::rt::TokioIo::new(stream), service)
            .into_owned();
        if let Err(error) = self.watcher.watch(connection).await {
            tracing::debug!(error = %error, "{ended}");
        }
    }
}

/// The service of one connection: the one router, with the connection's
/// address and client certificates put on each request.
fn connection_service(
    app: Router,
    peer: std::net::SocketAddr,
    peers: Option<crate::tls::PeerCertificates>,
    activity: Arc<keep_alive::Activity>,
) -> hyper_util::service::TowerToHyperService<WithConnection> {
    hyper_util::service::TowerToHyperService::new(WithConnection {
        app,
        peer,
        peers,
        activity,
    })
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
    activity: Arc<keep_alive::Activity>,
}

impl<B> tower::Service<http::Request<B>> for WithConnection
where
    B: hyper::body::Body,
    Router: tower::Service<http::Request<ReadToEnd<B>>, Response = axum::response::Response>,
{
    type Response = http::Response<keep_alive::Tracked>;
    type Error = <Router as tower::Service<http::Request<ReadToEnd<B>>>>::Error;
    type Future = CloseUnread<<Router as tower::Service<http::Request<ReadToEnd<B>>>>::Future>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<ReadToEnd<B>>>::poll_ready(&mut self.app, context)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let http1 = request.version() < http::Version::HTTP_2;
        if !http1 {
            self.activity.unbound();
        }
        let connect = request.method() == http::Method::CONNECT;
        let busy = http1.then(|| self.activity.begin());
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
            busy,
            connect,
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
    /// the request body was left unread and keeps the request in flight
    /// until its body is sent.
    struct CloseUnread<F> {
        #[pin]
        response: F,
        read: Option<Arc<AtomicBool>>,
        busy: Option<keep_alive::Busy>,
        connect: bool,
    }
}

impl<F, E> Future for CloseUnread<F>
where
    F: Future<Output = Result<axum::response::Response, E>>,
{
    type Output = Result<http::Response<keep_alive::Tracked>, E>;

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
        let busy = this.busy.take();
        let status = response.status();
        if let Some(busy) = &busy
            && (status == http::StatusCode::SWITCHING_PROTOCOLS
                || (*this.connect && status.is_success()))
        {
            busy.activity().unbound();
        }
        Poll::Ready(Ok(response.map(|body| keep_alive::Tracked::new(body, busy))))
    }
}
