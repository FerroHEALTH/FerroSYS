// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A handler panic becomes a `500` with the product's body, never a dropped
//! connection, and its message reaches neither the body nor the log.
//!
//! The release profile keeps `panic = "unwind"`, which is what lets the
//! panic be caught (<https://doc.rust-lang.org/cargo/reference/profiles.html#panic>).
//! [`caught`] drops the payload unread and marks the response; [`render`]
//! then answers with the body the product's [`PanicBody`] writes for the
//! request, and logs one fixed line with the server's request id. Rust's own
//! hook would print the message to stderr, so a binary installs
//! `ferrohealth_telemetry::panic::install_hook` at start.

use std::any::Any;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use ferrohealth_telemetry::request_log::RequestId;
use http::{StatusCode, Uri};

use crate::request_id::{self, RequestIds};

/// The body of a panicked request: the product writes it for the request's
/// URI and its [`RequestIds`], such as a FHIR `OperationOutcome` under its
/// FHIR base.
pub type PanicBody = Arc<dyn Fn(&Uri, RequestIds<'_>) -> Response + Send + Sync>;

/// A [`PanicBody`] from a body written for the URI and the server's request
/// id alone, the shape of `PanicBody` before 0.4.0.
#[must_use]
pub fn from_server_id<F>(body: F) -> PanicBody
where
    F: Fn(&Uri, &str) -> Response + Send + Sync + 'static,
{
    Arc::new(move |uri, ids| body(uri, ids.server))
}

/// The body a product that names none answers with: a JSON object naming the
/// failure and the server's request id. No specification governs its shape:
/// our own design.
#[must_use]
pub fn default_body() -> PanicBody {
    from_server_id(|_uri, request_id| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({
                "error": "internal",
                "message": "the server could not answer this request",
                "request_id": request_id,
            })),
        )
            .into_response()
    })
}

/// The marker [`caught`] leaves on the response for [`render`].
#[derive(Debug, Clone, Copy)]
struct Panicked;

/// The panic handler of `tower_http::catch_panic`: drops the payload unread
/// and answers `500` with the marker.
#[must_use]
pub fn caught(payload: Box<dyn Any + Send + 'static>) -> Response {
    drop(payload);
    let mut response = StatusCode::INTERNAL_SERVER_ERROR.into_response();
    response.extensions_mut().insert(Panicked);
    response
}

/// The middleware outside the panic catch: answers a marked response with
/// the product's body and logs one line naming the server's request id.
pub async fn render(body: PanicBody, request: Request, next: Next) -> Response {
    let uri = request.uri().clone();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.id().to_owned())
        .unwrap_or_default();
    let client = request_id::of_client(request.headers()).map(str::to_owned);
    let response = next.run(request).await;
    if response.extensions().get::<Panicked>().is_none() {
        return response;
    }
    answer(&body, &uri, RequestIds::new(&request_id, client.as_deref()))
}

/// Logs one line naming the server's request id and answers the panicked
/// request with the product's `body`.
pub(crate) fn answer(body: &PanicBody, uri: &Uri, ids: RequestIds<'_>) -> Response {
    tracing::error!(request_id = ids.server, "the request handler panicked");
    let mut rendered = body(uri, ids);
    *rendered.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
    rendered
}

#[cfg(test)]
mod tests {
    use super::{Panicked, caught};
    use http::StatusCode;

    #[test]
    fn a_caught_panic_is_a_500_carrying_the_marker() {
        let response = caught(Box::new("patient Qz7sentinel"));
        assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, response.status());
        assert!(
            response.extensions().get::<Panicked>().is_some(),
            "the renderer finds the marker"
        );
    }
}
