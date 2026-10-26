// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The body of every refusal the stack writes itself, in the product's shape.
//!
//! The stack refuses a request that runs past the request timeout (`408`),
//! one whose declared body is larger than the limit (`413`), one past the
//! overload bound (`503`) and one past its caller's rate (`429`). A product
//! answers every error in one shape, such as a FHIR `OperationOutcome` (FHIR
//! R4, `http.html`), so [`render`] writes each of these refusals
//! with the product's [`RefusalBody`], keeping the status and the
//! `Retry-After` header. A response a handler wrote is never rewritten, a
//! `413` from a body extractor included. No specification governs the rest:
//! our own design.

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use ferrohealth_telemetry::request_log::RequestId;
use http::{StatusCode, Uri, header};

/// The body of a refusal: the product writes it for the request's URI, the
/// server's request id, the status and a reason in plain words.
pub type RefusalBody = Arc<dyn Fn(&Uri, &str, StatusCode, &str) -> Response + Send + Sync>;

/// The body a product that names none answers with: the reason as plain
/// text.
#[must_use]
pub fn default_body() -> RefusalBody {
    Arc::new(|_uri, _request_id, status, reason| (status, reason.to_owned()).into_response())
}

/// The reason of a request that ran past the request timeout.
pub const TIMEOUT: &str = "the request took longer than the server allows";

/// The reason of a body larger than the limit.
pub const TOO_LARGE: &str = "the request body is larger than the server reads";

/// The marker of a refusal the stack wrote, with its reason.
#[derive(Debug, Clone, Copy)]
struct Refused(&'static str);

/// The marker of a response that came from inside the timeout and the body
/// limit, which a handler or the routes wrote.
#[derive(Debug, Clone, Copy)]
struct Answered;

/// A refusal of `status` for `reason`, marked for [`render`].
pub(crate) fn refused(status: StatusCode, reason: &'static str) -> Response {
    let mut response = (status, reason).into_response();
    response.extensions_mut().insert(Refused(reason));
    response
}

/// The middleware inside the timeout and the body limit: marks every
/// response that passed it, so [`render`] knows the others are theirs.
pub async fn answered(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.extensions_mut().insert(Answered);
    response
}

/// The middleware outside the panic catch: writes each refusal of the stack
/// with `body`.
pub async fn render(body: RefusalBody, request: Request, next: Next) -> Response {
    let uri = request.uri().clone();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.id().to_owned())
        .unwrap_or_default();
    let response = next.run(request).await;
    let status = response.status();
    let reason = match reason(&response) {
        Some(reason) => reason,
        None if response.extensions().get::<Answered>().is_some() => return response,
        None if status == StatusCode::REQUEST_TIMEOUT => TIMEOUT,
        None if status == StatusCode::PAYLOAD_TOO_LARGE => TOO_LARGE,
        None => return response,
    };
    rewrite(&body, &uri, &request_id, &response, reason)
}

/// The reason of a refusal [`refused`] wrote, when `response` is one.
pub(crate) fn reason<B>(response: &http::Response<B>) -> Option<&'static str> {
    response
        .extensions()
        .get::<Refused>()
        .map(|Refused(reason)| *reason)
}

/// The refusal `response` written with `body` for `reason`, keeping its
/// status and its `Retry-After`.
pub(crate) fn rewrite<B>(
    body: &RefusalBody,
    uri: &Uri,
    request_id: &str,
    response: &http::Response<B>,
    reason: &str,
) -> Response {
    let status = response.status();
    let mut rendered = body(uri, request_id, status, reason);
    *rendered.status_mut() = status;
    if let Some(wait) = response.headers().get(header::RETRY_AFTER) {
        rendered
            .headers_mut()
            .insert(header::RETRY_AFTER, wait.clone());
    }
    rendered
}

/// A refusal of `status` for `reason` the stack writes before any handler
/// runs, such as a timeout, written with `body`.
pub(crate) fn write(
    body: &RefusalBody,
    uri: &Uri,
    request_id: &str,
    status: StatusCode,
    reason: &str,
) -> Response {
    let mut rendered = body(uri, request_id, status, reason);
    *rendered.status_mut() = status;
    rendered
}
