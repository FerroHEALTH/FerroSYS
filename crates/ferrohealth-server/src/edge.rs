// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The outer services of the stack: [`Identify`] mints the request id, and
//! [`Edge`], inside the request log, reads the client address, catches a
//! panic, bounds the request time and the declared body, admits the
//! request's content coding when decoding is on, and renders every refusal
//! and panic, as one service with one unboxed future.
//!
//! Each of these was its own layer, and a stack of `axum::middleware`
//! layers boxes a future and clones the services inside it for every
//! request. The request id is minted outside the request log, so its line
//! names it, and everything else is inside, so every answer the edge writes
//! leaves its line. No specification governs this: our own design.

use std::any::Any;
use std::future::Future;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::BoxError;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::ConnectInfo;
use axum::response::Response;
use ferrohealth_telemetry::request_log::RequestId;
use http::{HeaderValue, Request, StatusCode, Uri};
use http_body_util::LengthLimitError;
use hyper::body::{Frame, SizeHint};
use tokio::time::{Instant, Sleep};

use crate::client_address::{ClientAddress, Forwarding};
use crate::decoding::{self, Watch};
use crate::panic::{self, PanicBody};
use crate::refusal::{self, RefusalBody};
use crate::request_id::{self, RequestIds};

/// What the outer service holds, shared by its clones.
pub(crate) struct Settings {
    /// The trusted proxies and their header.
    pub(crate) forwarding: Forwarding,
    /// The body of a panicked request.
    pub(crate) panic_body: PanicBody,
    /// The body of a refusal.
    pub(crate) refusal_body: RefusalBody,
    /// The request timeout.
    pub(crate) timeout: Duration,
    /// The body limit, in bytes.
    pub(crate) body_limit: usize,
    /// Whether a gzip or Brotli request body is decoded.
    pub(crate) decoding: bool,
}

/// The layer that mints the request id, outside the request log.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct IdentifyLayer;

impl<S> tower::Layer<S> for IdentifyLayer {
    type Service = Identify<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Identify { inner }
    }
}

/// Mints the server's request id into the request's extensions and drops
/// an illegal client id; [`Edge`] echoes the one or the other.
#[derive(Clone)]
pub(crate) struct Identify<S> {
    inner: S,
}

impl<S, B> tower::Service<Request<B>> for Identify<S>
where
    S: tower::Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let client_named = request_id::client_value(request.headers()).is_some();
        // NOTE: no specification governs this: our own design; an illegal client id
        // is dropped before any handler sees it, so nothing downstream reads it.
        if !client_named {
            request.headers_mut().remove(request_id::HEADER);
        }
        request
            .extensions_mut()
            .insert(request_id::mint_id(client_named));
        self.inner.call(request)
    }
}

/// The layer of the outer service.
#[derive(Clone)]
pub(crate) struct EdgeLayer(Arc<Settings>);

impl EdgeLayer {
    /// The layer over `settings`.
    pub(crate) fn new(settings: Settings) -> Self {
        Self(Arc::new(settings))
    }
}

impl<S> tower::Layer<S> for EdgeLayer {
    type Service = Edge<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Edge {
            settings: Arc::clone(&self.0),
            inner,
        }
    }
}

/// The outer service.
#[derive(Clone)]
pub(crate) struct Edge<S> {
    settings: Arc<Settings>,
    inner: S,
}

/// What the answer to one request is written with.
struct Answer {
    settings: Arc<Settings>,
    uri: Uri,
    id: RequestId,
    client: Option<HeaderValue>,
    /// Set once a body with no declared length, or a decoded body, passed
    /// the limit.
    over_limit: Option<Arc<AtomicBool>>,
    /// Set once an encoded body failed to decode.
    failed: Option<Arc<AtomicBool>>,
}

impl Answer {
    /// Echoes the client's id, or the server's, on `response`, and puts the
    /// server's id in its extensions.
    fn finish(&self, mut response: Response) -> Response {
        let echoed = match &self.client {
            Some(client) => Some(client.clone()),
            None => HeaderValue::from_str(self.id.id()).ok(),
        };
        if let Some(echoed) = echoed {
            response.headers_mut().insert(request_id::HEADER, echoed);
        }
        response.extensions_mut().insert(self.id.clone());
        response
    }

    /// The server's id and the one [`Answer::finish`] echoes.
    fn ids(&self) -> RequestIds<'_> {
        RequestIds::new(
            self.id.id(),
            self.client.as_ref().and_then(|client| client.to_str().ok()),
        )
    }

    /// A refusal of `status` for `reason` the stack writes itself.
    fn refusal(&self, status: StatusCode, reason: &str) -> Response {
        self.finish(refusal::write(
            &self.settings.refusal_body,
            &self.uri,
            self.ids(),
            status,
            reason,
        ))
    }
}

impl<S, ResB> tower::Service<Request<Body>> for Edge<S>
where
    S: tower::Service<Request<Body>, Response = http::Response<ResB>>,
    ResB: HttpBody<Data = Bytes> + Send + 'static,
    ResB::Error: Into<BoxError>,
{
    type Response = Response;
    type Error = S::Error;
    type Future = EdgeFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let client = request_id::client_value(request.headers());
        let id = if let Some(id) = request.extensions().get::<RequestId>() {
            id.clone()
        } else {
            if client.is_none() {
                request.headers_mut().remove(request_id::HEADER);
            }
            let id = request_id::mint_id(client.is_some());
            request.extensions_mut().insert(id.clone());
            id
        };
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(peer)| peer.ip());
        if let Some(peer) = peer {
            let address = self.settings.forwarding.client(peer, request.headers());
            request.extensions_mut().insert(ClientAddress(address));
        }
        // NOTE: RFC 9110 §15.5.14, a declared length past the limit is refused
        // before the body is read; a streamed body is cut by the limit below.
        let declared = request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
        let limit = self.settings.body_limit;
        let watched_limit = u64::try_from(limit).unwrap_or(u64::MAX);
        let coding = if self.settings.decoding {
            decoding::admit(request.headers_mut())
        } else {
            Ok(false)
        };
        let decoded = if coding.as_ref().is_ok_and(|encoded| *encoded) {
            let watch = Watch {
                limit,
                over: Arc::new(AtomicBool::new(false)),
                failed: Arc::new(AtomicBool::new(false)),
            };
            request.extensions_mut().insert(watch.clone());
            Some(watch)
        } else {
            None
        };
        let over_limit = match declared {
            Some(_) => None,
            None if request
                .body()
                .size_hint()
                .upper()
                .is_some_and(|upper| upper <= watched_limit) =>
            {
                None
            }
            None => {
                let over = decoded.as_ref().map_or_else(
                    || Arc::new(AtomicBool::new(false)),
                    |watch| Arc::clone(&watch.over),
                );
                let flag = Arc::clone(&over);
                request = request.map(|body| Body::new(Watched::new(body, watched_limit, flag)));
                Some(over)
            }
        };
        let over_limit =
            over_limit.or_else(|| decoded.as_ref().map(|watch| Arc::clone(&watch.over)));
        let answer = Answer {
            settings: Arc::clone(&self.settings),
            uri: request.uri().clone(),
            id,
            client,
            over_limit,
            failed: decoded.map(|watch| watch.failed),
        };
        if coding.is_err() {
            return EdgeFuture {
                state: State::Unsupported,
                answer,
            };
        }
        if declared.is_some_and(|length| length > limit) {
            return EdgeFuture {
                state: State::TooLarge,
                answer,
            };
        }
        let deadline = Instant::now().checked_add(self.settings.timeout);
        let state = match std::panic::catch_unwind(AssertUnwindSafe(|| self.inner.call(request))) {
            Ok(inner) => State::Running {
                inner,
                deadline,
                sleep: None,
            },
            Err(payload) => State::Panicked {
                payload: Some(payload),
            },
        };
        EdgeFuture { state, answer }
    }
}

pin_project_lite::pin_project! {
    /// The future of [`Edge`].
    pub(crate) struct EdgeFuture<F> {
        #[pin]
        state: State<F>,
        answer: Answer,
    }
}

pin_project_lite::pin_project! {
    #[project = StateProjection]
    enum State<F> {
        Running {
            #[pin]
            inner: F,
            deadline: Option<Instant>,
            #[pin]
            sleep: Option<Sleep>,
        },
        Panicked {
            payload: Option<Box<dyn Any + Send + 'static>>,
        },
        TooLarge,
        Unsupported,
    }
}

impl<F, ResB, E> Future for EdgeFuture<F>
where
    F: Future<Output = Result<http::Response<ResB>, E>>,
    ResB: HttpBody<Data = Bytes> + Send + 'static,
    ResB::Error: Into<BoxError>,
{
    type Output = Result<Response, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let answer = &*this.answer;
        match this.state.project() {
            StateProjection::Running {
                mut inner,
                deadline,
                mut sleep,
            } => match std::panic::catch_unwind(AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
                Ok(Poll::Ready(Ok(response))) => {
                    Poll::Ready(Ok(answer.finish(answered(answer, response.map(Body::new)))))
                }
                Ok(Poll::Ready(Err(error))) => Poll::Ready(Err(error)),
                Err(payload) => {
                    drop(payload);
                    Poll::Ready(Ok(panicked(answer)))
                }
                Ok(Poll::Pending) => {
                    let Some(deadline) = *deadline else {
                        return Poll::Pending;
                    };
                    // NOTE: no specification governs this: our own design; the timer is
                    // armed only once the handler waits, so an immediate answer never
                    // touches the timer wheel.
                    if sleep.is_none() {
                        sleep.set(Some(tokio::time::sleep_until(deadline)));
                    }
                    match sleep.as_pin_mut().map(|sleep| sleep.poll(cx)) {
                        Some(Poll::Ready(())) => Poll::Ready(Ok(
                            answer.refusal(StatusCode::REQUEST_TIMEOUT, refusal::TIMEOUT)
                        )),
                        _ => Poll::Pending,
                    }
                }
            },
            StateProjection::Panicked { payload } => {
                drop(payload.take());
                Poll::Ready(Ok(panicked(answer)))
            }
            StateProjection::TooLarge => Poll::Ready(Ok(
                answer.refusal(StatusCode::PAYLOAD_TOO_LARGE, refusal::TOO_LARGE)
            )),
            StateProjection::Unsupported => {
                // NOTE: RFC 7694 §3, a 415 for a content coding names the codings the server
                // accepts in Accept-Encoding.
                let mut refused = answer.refusal(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    refusal::UNSUPPORTED_CODING,
                );
                refused
                    .headers_mut()
                    .insert(http::header::ACCEPT_ENCODING, decoding::ACCEPTED);
                Poll::Ready(Ok(refused))
            }
        }
    }
}

/// A response from inside the stack: a refusal [`refusal::refused`] marked,
/// a `413` raised once a streamed or decoded body passed the limit, and a
/// `400` raised once an encoded body failed to decode, are written in the
/// product's shape, and any other is the handler's own.
fn answered(answer: &Answer, response: Response) -> Response {
    let reason = refusal::reason(&response).or_else(|| {
        if Arc::ptr_eq(&answer.settings.refusal_body, &refusal::DEFAULT_BODY) {
            return None;
        }
        let marked = |flag: &Option<Arc<AtomicBool>>| {
            flag.as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
        };
        match response.status() {
            StatusCode::PAYLOAD_TOO_LARGE if marked(&answer.over_limit) => Some(refusal::TOO_LARGE),
            StatusCode::BAD_REQUEST if marked(&answer.failed) => Some(refusal::UNDECODABLE),
            _ => None,
        }
    });
    match reason {
        Some(reason) => refusal::rewrite(
            &answer.settings.refusal_body,
            &answer.uri,
            answer.ids(),
            &response,
            reason,
        ),
        None => response,
    }
}

pin_project_lite::pin_project! {
    /// A request body that marks when more than the limit has been read
    /// from it: a body with no declared length, which the body limit inside
    /// the edge refuses, or a decoded body inside its own limit, which also
    /// marks a stream that failed to decode and ends at its first error.
    pub(crate) struct Watched<B> {
        #[pin]
        inner: B,
        seen: u64,
        limit: u64,
        over: Arc<AtomicBool>,
        failed: Option<Arc<AtomicBool>>,
        ended: bool,
    }
}

impl<B> Watched<B> {
    /// Watches `inner` against `limit` bytes, marking `over`.
    fn new(inner: B, limit: u64, over: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            seen: 0,
            limit,
            over,
            failed: None,
            ended: false,
        }
    }

    /// Watches the decoded body `inner` for the edge's `watch`.
    pub(crate) fn decoded(inner: B, watch: Watch) -> Self {
        Self {
            failed: Some(watch.failed),
            ..Self::new(
                inner,
                u64::try_from(watch.limit).unwrap_or(u64::MAX),
                watch.over,
            )
        }
    }
}

impl<B> HttpBody for Watched<B>
where
    B: HttpBody<Data = Bytes>,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        if *this.ended {
            return Poll::Ready(None);
        }
        let polled = match std::task::ready!(this.inner.poll_frame(cx)) {
            Some(Ok(frame)) => Some(Ok(frame)),
            Some(Err(error)) => Some(Err(error.into())),
            None => None,
        };
        match &polled {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    *this.seen = this
                        .seen
                        .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
                    if *this.seen > *this.limit {
                        this.over.store(true, Ordering::Relaxed);
                    }
                }
            }
            Some(Err(error)) => {
                if let Some(failed) = this.failed {
                    *this.ended = true;
                    // NOTE: no specification governs this: our own design; the decoder hands
                    // back the wire limit's error as it is, so both limits read as one `413`.
                    let mark = if error.is::<LengthLimitError>() {
                        &*this.over
                    } else {
                        &**failed
                    };
                    mark.store(true, Ordering::Relaxed);
                }
            }
            None => {}
        }
        Poll::Ready(polled)
    }

    fn is_end_stream(&self) -> bool {
        self.ended || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The answer to a request whose handler panicked; the payload is dropped
/// unread.
fn panicked(answer: &Answer) -> Response {
    answer.finish(panic::answer(
        &answer.settings.panic_body,
        &answer.uri,
        answer.ids(),
    ))
}
