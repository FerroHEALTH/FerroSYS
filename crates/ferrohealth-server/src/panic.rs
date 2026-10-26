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

/// The body of a panicked request: the product writes it for the request's
/// URI and the server's request id, such as a FHIR `OperationOutcome` under
/// its FHIR base.
pub type PanicBody = Arc<dyn Fn(&Uri, &str) -> Response + Send + Sync>;

/// The body a product that names none answers with: a JSON object naming the
/// failure and the server's request id. No specification governs its shape:
/// our own design.
#[must_use]
pub fn default_body() -> PanicBody {
    Arc::new(|_uri, request_id| {
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
    let response = next.run(request).await;
    if response.extensions().get::<Panicked>().is_none() {
        return response;
    }
    tracing::error!(
        request_id = request_id.as_str(),
        "the request handler panicked"
    );
    let mut rendered = body(&uri, &request_id);
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
