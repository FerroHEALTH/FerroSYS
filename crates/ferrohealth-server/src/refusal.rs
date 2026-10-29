// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The body of every refusal the stack writes itself, in the product's shape.
//!
//! The stack refuses a request that runs past the request timeout (`408`),
//! one whose declared body is larger than the limit (`413`), one past the
//! overload bound (`503`) and one past its caller's rate (`429`). A product
//! answers every error in one shape, such as a FHIR `OperationOutcome` (FHIR
//! R4, `http.html`), so [`render`] writes each of these refusals
//! with the product's [`RefusalBody`], keeping the `Retry-After` header and
//! the status, unless the body chose its own with [`keep_status`]. A `413`
//! a body extractor raises because the body passed the limit is written with
//! a product's body too; any other response a handler wrote is never
//! rewritten. No specification governs the rest: our own design.

use std::sync::{Arc, LazyLock};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use ferrohealth_telemetry::request_log::RequestId;
use http::{StatusCode, Uri, header};

use crate::request_id::{self, RequestIds};

/// The body of a refusal: the product writes it for the request's URI, its
/// [`RequestIds`], the status and a reason in plain words.
pub type RefusalBody =
    Arc<dyn Fn(&Uri, RequestIds<'_>, StatusCode, &str) -> Response + Send + Sync>;

/// A [`RefusalBody`] from a body written for the URI, the server's request
/// id, the status and the reason, the shape of `RefusalBody` before 0.4.0.
#[must_use]
pub fn from_server_id<F>(body: F) -> RefusalBody
where
    F: Fn(&Uri, &str, StatusCode, &str) -> Response + Send + Sync + 'static,
{
    Arc::new(move |uri, ids, status, reason| body(uri, ids.server, status, reason))
}

/// The one default body, so the stack can tell it from a product's.
static DEFAULT_BODY: LazyLock<RefusalBody> = LazyLock::new(|| {
    Arc::new(|_uri, _ids, status, reason| (status, reason.to_owned()).into_response())
});

/// The body a product that names none answers with: the reason as plain
/// text.
///
/// A `413` a body extractor raises keeps the extractor's own text under
/// this body, as it did before a product's body could write it.
#[must_use]
pub fn default_body() -> RefusalBody {
    Arc::clone(&DEFAULT_BODY)
}

/// Whether `body` is the [`default_body`].
pub(crate) fn is_default(body: &RefusalBody) -> bool {
    Arc::ptr_eq(body, &DEFAULT_BODY)
}

/// Marks a response a [`RefusalBody`] rendered as answering with its own
/// status, such as a FHIR gateway's `504 Gateway Timeout` for the stack's
/// `408`; without the mark the stack sets the status it refused with.
///
/// The stack still adds the `Retry-After` of the refusal and the request id
/// header.
#[must_use]
pub fn keep_status(mut response: Response) -> Response {
    response.extensions_mut().insert(KeepStatus);
    response
}

/// The mark [`keep_status`] leaves.
#[derive(Debug, Clone, Copy)]
struct KeepStatus;

/// Sets `status` on `rendered` unless the body kept its own.
fn set_status(rendered: &mut Response, status: StatusCode) {
    if rendered.extensions_mut().remove::<KeepStatus>().is_none() {
        *rendered.status_mut() = status;
    }
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
    let client = request_id::of_client(request.headers()).map(str::to_owned);
    let response = next.run(request).await;
    let status = response.status();
    let reason = match reason(&response) {
        Some(reason) => reason,
        None if response.extensions().get::<Answered>().is_some() => return response,
        None if status == StatusCode::REQUEST_TIMEOUT => TIMEOUT,
        None if status == StatusCode::PAYLOAD_TOO_LARGE => TOO_LARGE,
        None => return response,
    };
    rewrite(
        &body,
        &uri,
        RequestIds::new(&request_id, client.as_deref()),
        &response,
        reason,
    )
}

/// The reason of a refusal [`refused`] wrote, when `response` is one.
pub(crate) fn reason<B>(response: &http::Response<B>) -> Option<&'static str> {
    response
        .extensions()
        .get::<Refused>()
        .map(|Refused(reason)| *reason)
}

/// The refusal `response` written with `body` for `reason`, keeping its
/// `Retry-After` and its status, unless the body kept its own.
pub(crate) fn rewrite<B>(
    body: &RefusalBody,
    uri: &Uri,
    ids: RequestIds<'_>,
    response: &http::Response<B>,
    reason: &str,
) -> Response {
    let status = response.status();
    let mut rendered = body(uri, ids, status, reason);
    set_status(&mut rendered, status);
    if let Some(wait) = response.headers().get(header::RETRY_AFTER) {
        rendered
            .headers_mut()
            .insert(header::RETRY_AFTER, wait.clone());
    }
    rendered
}

/// A refusal of `status` for `reason` the stack writes before any handler
/// runs, such as a timeout, written with `body`; the status is `status`,
/// unless the body kept its own.
pub(crate) fn write(
    body: &RefusalBody,
    uri: &Uri,
    ids: RequestIds<'_>,
    status: StatusCode,
    reason: &str,
) -> Response {
    let mut rendered = body(uri, ids, status, reason);
    set_status(&mut rendered, status);
    rendered
}
