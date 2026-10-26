// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The outer service of the stack: the request id, the client address, the
//! panic catch, the request timeout, the declared body limit and the
//! rendering of every refusal and panic, as one service with one unboxed
//! future.
//!
//! Each of these was its own layer, and a stack of `axum::middleware`
//! layers boxes a future and clones the services inside it for every
//! request. No specification governs this: our own design.

use std::any::Any;
use std::future::Future;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::BoxError;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::ConnectInfo;
use axum::response::Response;
use ferrohealth_telemetry::request_log::RequestId;
use http::{HeaderValue, Request, StatusCode, Uri};
use tokio::time::{Instant, Sleep};

use crate::client_address::{ClientAddress, Forwarding};
use crate::panic::{self, PanicBody};
use crate::refusal::{self, RefusalBody};
use crate::request_id;

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

    /// A refusal of `status` for `reason` the stack writes itself.
    fn refusal(&self, status: StatusCode, reason: &str) -> Response {
        self.finish(refusal::write(
            &self.settings.refusal_body,
            &self.uri,
            self.id.id(),
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
        // NOTE: no specification governs this: our own design; an illegal client id
        // is dropped before any handler sees it, so nothing downstream reads it.
        if client.is_none() {
            request.headers_mut().remove(request_id::HEADER);
        }
        let id = request_id::mint_id(client.is_some());
        request.extensions_mut().insert(id.clone());
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(peer)| peer.ip());
        if let Some(peer) = peer {
            let address = self.settings.forwarding.client(peer, request.headers());
            request.extensions_mut().insert(ClientAddress(address));
        }
        let answer = Answer {
            settings: Arc::clone(&self.settings),
            uri: request.uri().clone(),
            id,
            client,
        };
        // NOTE: RFC 9110 §15.5.14, a declared length past the limit is refused
        // before the body is read; a streamed body is cut by the limit below.
        let declared = request
            .headers()
            .get(http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()?.parse::<usize>().ok());
        if declared.is_some_and(|length| length > self.settings.body_limit) {
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
        }
    }
}

/// A response from inside the stack: a refusal [`refusal::refused`] marked
/// is written in the product's shape, and any other is the handler's own.
fn answered(answer: &Answer, response: Response) -> Response {
    match refusal::reason(&response) {
        Some(reason) => refusal::rewrite(
            &answer.settings.refusal_body,
            &answer.uri,
            answer.id.id(),
            &response,
            reason,
        ),
        None => response,
    }
}

/// The answer to a request whose handler panicked; the payload is dropped
/// unread.
fn panicked(answer: &Answer) -> Response {
    answer.finish(panic::answer(
        &answer.settings.panic_body,
        &answer.uri,
        answer.id.id(),
    ))
}
