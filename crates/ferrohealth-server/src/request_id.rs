// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The request id: the server's own for every request, and the client's
//! echoed and never logged.
//!
//! The server mints a fresh id for every request and puts it in the
//! request's extensions as [`RequestId`], which the request log and every
//! outbound call read. A client may send `X-Request-Id` to correlate its
//! own logs: a printable ASCII value of bounded length is echoed on the
//! response, and is never logged, because it is free text that may name a
//! patient. Without one, the response carries the server's id.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use ferrohealth_telemetry::request_log::RequestId;
use http::{HeaderMap, HeaderName, HeaderValue};

/// The header a request id travels in.
pub const HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// The longest client id the server echoes.
pub const MAX_LENGTH: usize = 128;

/// Whether `value` is a client id the server echoes: non-empty, at most
/// [`MAX_LENGTH`] bytes, printable ASCII or a space, so it cannot forge a
/// header or a log line.
#[must_use]
pub fn is_legal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LENGTH
        && value.chars().all(|c| c.is_ascii_graphic() || c == ' ')
}

/// Returns a fresh server id.
#[must_use]
pub fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Returns the client's id in `headers`, when it is legal.
#[must_use]
pub fn of_client(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_legal(value))
}

/// The middleware: mints the server's id, records whether the client named
/// one, and echoes the client's id or the server's on the response.
pub async fn assign(mut request: Request, next: Next) -> Response {
    let client = of_client(request.headers()).map(str::to_owned);
    // NOTE: no specification governs this: our own design; an illegal client id
    // is dropped before any handler sees it, so nothing downstream reads it.
    if client.is_none() {
        request.headers_mut().remove(HEADER);
    }
    let id = RequestId::new(mint(), client.is_some());
    request.extensions_mut().insert(id.clone());
    let mut response = next.run(request).await;
    let echoed = client.as_deref().unwrap_or(id.id());
    if let Ok(value) = HeaderValue::from_str(echoed) {
        response.headers_mut().insert(HEADER, value);
    }
    response.extensions_mut().insert(id);
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::routing::get;
    use ferrohealth_telemetry::request_log::RequestId;
    use tower::ServiceExt as _;

    use super::{HEADER, MAX_LENGTH, assign, is_legal};

    fn app() -> Router {
        Router::new()
            .route(
                "/",
                get(
                    |Extension(id): Extension<RequestId>, headers: http::HeaderMap| async move {
                        format!(
                            "{} {} {}",
                            id.id().len(),
                            id.client_named(),
                            headers.get(HEADER).is_some()
                        )
                    },
                ),
            )
            .layer(axum::middleware::from_fn(assign))
    }

    async fn call(id: Option<&str>) -> (String, String) {
        let mut request = http::Request::builder().uri("/");
        if let Some(id) = id {
            request = request.header(HEADER, id);
        }
        let response = app()
            .oneshot(request.body(Body::empty()).expect("the request builds"))
            .await
            .expect("the request is served");
        let echoed = response
            .headers()
            .get(HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("the body");
        (echoed, String::from_utf8(body.to_vec()).expect("UTF-8"))
    }

    #[tokio::test]
    async fn a_client_id_is_echoed_and_the_server_keeps_its_own() {
        let (echoed, body) = call(Some("corr-42")).await;
        assert_eq!("corr-42", echoed);
        assert_eq!(
            "36 true true", body,
            "the server's id is a UUID, and the client named one"
        );
    }

    #[tokio::test]
    async fn without_a_client_id_the_server_id_is_echoed() {
        let (echoed, body) = call(None).await;
        assert_eq!(36, echoed.len(), "{echoed}");
        assert_eq!("36 false false", body);
    }

    #[tokio::test]
    async fn an_illegal_client_id_is_dropped_before_the_handler() {
        let (echoed, body) = call(Some("corr\u{e9}42")).await;
        assert_eq!(36, echoed.len(), "the server's id replaces it: {echoed}");
        assert_eq!("36 false false", body);
    }

    #[test]
    fn a_legal_id_is_printable_ascii_of_bounded_length() {
        assert!(is_legal("corr-42"));
        assert!(is_legal("a b"));
        assert!(is_legal(&"a".repeat(MAX_LENGTH)));
        for bad in [
            "",
            "corr\n42",
            "corr\r42",
            "corr\t42",
            "corr\u{7f}42",
            "corr\u{e9}42",
        ] {
            assert!(!is_legal(bad), "{bad:?} is refused");
        }
        assert!(!is_legal(&"a".repeat(MAX_LENGTH + 1)));
    }
}
