// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The request lines the stack writes, the bodies a product answers with
//! and the problem documents.

use std::num::NonZeroUsize;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse as _;
use http::StatusCode;

use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{StackPolicy, app};

use super::tracing_subscriber_shim::Capture;
use super::{get_status, product, refusing, section, send, serving, waiting};

fn capturing() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_writer(capture.clone())
        .finish();
    (capture, tracing::subscriber::set_default(subscriber))
}

fn request_lines(capture: &Capture) -> Vec<serde_json::Value> {
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    log.lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .filter(|line: &serde_json::Value| line["message"] == "request")
        .collect()
}

/// Sends `request` and asserts it left exactly one request line with
/// `route`, `status`, a latency and the id the response echoes.
async fn one_line(app: Router, request: http::Request<Body>, route: &str, status: StatusCode) {
    let (capture, _default) = capturing();
    let (got, headers, _) = send(app, request).await;
    assert_eq!(status, got, "{route}");
    let echoed = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let lines = request_lines(&capture);
    assert_eq!(1, lines.len(), "one line for {route}: {lines:?}");
    let line = lines.first().expect("one line");
    assert!(
        line["method"].as_str().is_some_and(|m| !m.is_empty()),
        "{line}"
    );
    assert_eq!(Some(route), line["route"].as_str(), "{line}");
    assert_eq!(Some(u64::from(status.as_u16())), line["status"].as_u64());
    assert!(line["latency_ms"].as_f64().is_some(), "{line}");
    assert_eq!(36, echoed.len(), "{echoed}");
    assert_eq!(Some(echoed.as_str()), line["request_id"].as_str());
}

fn get_request(uri: &str) -> http::Request<Body> {
    http::Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("the request builds")
}

#[tokio::test(start_paused = true)]
async fn a_timeout_leaves_its_request_line() {
    for policy in [StackPolicy::default(), refusing()] {
        let app = app(product(), serving(), &section(), policy);
        one_line(
            app,
            get_request("/ferrosys/slow"),
            "/ferrosys/slow",
            StatusCode::REQUEST_TIMEOUT,
        )
        .await;
    }
}

#[tokio::test]
async fn a_refused_oversized_body_leaves_its_request_line() {
    for policy in [StackPolicy::default(), refusing()] {
        let app = app(product(), serving(), &section(), policy);
        let declared = http::Request::builder()
            .method("POST")
            .uri("/ferrosys/echo")
            .header(http::header::CONTENT_LENGTH, "33")
            .body(Body::from("a body of more than sixteen bytes"))
            .expect("the request builds");
        one_line(
            app.clone(),
            declared,
            "/ferrosys/echo",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
        .await;
        let streamed = http::Request::builder()
            .method("POST")
            .uri("/ferrosys/echo")
            .body(Body::from("a body of more than sixteen bytes"))
            .expect("the request builds");
        one_line(
            app,
            streamed,
            "/ferrosys/echo",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
        .await;
    }
}

#[tokio::test]
async fn a_caught_panic_leaves_its_request_line() {
    for policy in [StackPolicy::default(), refusing()] {
        let app = app(product().merge(waiting()), serving(), &section(), policy);
        for uri in ["/ferrosys/boom", "/ferrosys/late-boom"] {
            one_line(
                app.clone(),
                get_request(uri),
                uri,
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .await;
        }
    }
}

fn gateway() -> StackPolicy {
    StackPolicy {
        refusal_body: ferrohealth_server::refusal::from_server_id(
            |_uri: &http::Uri, _request_id: &str, status: StatusCode, reason: &str| {
                let chosen = if status == StatusCode::REQUEST_TIMEOUT {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                };
                ferrohealth_server::refusal::keep_status(
                    (
                        chosen,
                        axum::Json(
                            serde_json::json!({ "refused": status.as_u16(), "reason": reason }),
                        ),
                    )
                        .into_response(),
                )
            },
        ),
        ..StackPolicy::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_refusal_body_may_answer_its_own_status() {
    let app = app(product(), serving(), &section(), gateway());
    let (capture, _default) = capturing();
    let (status, headers, body) = get_status(app, "/ferrosys/slow").await;
    assert_eq!(StatusCode::GATEWAY_TIMEOUT, status);
    assert_eq!(
        36,
        headers
            .get("x-request-id")
            .map_or(0, http::HeaderValue::len)
    );
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(408, body["refused"]);
    let lines = request_lines(&capture);
    assert_eq!(
        vec![Some(504)],
        lines
            .iter()
            .map(|line| line["status"].as_u64())
            .collect::<Vec<_>>(),
        "the line names the status the client got"
    );
}

#[tokio::test]
async fn a_refusal_that_answers_its_own_status_keeps_its_retry_after() {
    let section = ServerSection {
        max_concurrent_requests: NonZeroUsize::new(1),
        retry_after_s: 2,
        ..section()
    };
    let app = app(product(), serving(), &section, gateway());
    let held = tokio::spawn(get_status(app.clone(), "/ferrosys/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let request = http::Request::builder()
        .uri("/ferrosys/fhir/Patient/7")
        .header("x-request-id", "corr-42")
        .body(Body::empty())
        .expect("the request builds");
    let (status, headers, _) = send(app, request).await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(
        (Some("2"), Some("corr-42")),
        (
            headers.get("retry-after").and_then(|v| v.to_str().ok()),
            headers.get("x-request-id").and_then(|v| v.to_str().ok()),
        )
    );
    let _held = held.await.expect("the held request ends");
}

#[tokio::test]
async fn without_the_mark_the_stack_keeps_its_own_status() {
    let policy = StackPolicy {
        refusal_body: ferrohealth_server::refusal::from_server_id(
            |_uri: &http::Uri, _id: &str, _status: StatusCode, _reason: &str| {
                (StatusCode::GATEWAY_TIMEOUT, "unmarked").into_response()
            },
        ),
        ..StackPolicy::default()
    };
    let app = app(product(), serving(), &section(), policy);
    let request = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .header(http::header::CONTENT_LENGTH, "33")
        .body(Body::from("a body of more than sixteen bytes"))
        .expect("the request builds");
    let (status, _, body) = send(app, request).await;
    assert_eq!(
        (StatusCode::PAYLOAD_TOO_LARGE, "unmarked"),
        (status, body.as_str())
    );
}

#[tokio::test]
async fn the_default_body_keeps_the_extractors_own_413() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let request = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .body(Body::from("a body of more than sixteen bytes"))
        .expect("the request builds");
    let (status, _, body) = send(app, request).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    assert_ne!(
        ferrohealth_server::refusal::TOO_LARGE,
        body,
        "the extractor's own text"
    );
}

#[tokio::test]
async fn a_streamed_body_inside_the_limit_is_read_whole() {
    let app = app(product(), serving(), &section(), refusing());
    let request = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .body(Body::new(Chunks(vec!["bytes!!!", "sixteen "])))
        .expect("the request builds");
    let (status, _, body) = send(app, request).await;
    assert_eq!((StatusCode::OK, "16"), (status, body.as_str()));
}

/// A body of chunks with no length, popped from the end.
struct Chunks(Vec<&'static str>);

impl axum::body::HttpBody for Chunks {
    type Data = axum::body::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Ready(
            self.get_mut()
                .0
                .pop()
                .map(|chunk| Ok(hyper::body::Frame::data(axum::body::Bytes::from(chunk)))),
        )
    }
}

#[tokio::test]
async fn a_streamed_body_past_the_limit_in_chunks_is_written_with_the_product_body() {
    let app = app(product(), serving(), &section(), refusing());
    let request = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .body(Body::new(Chunks(vec!["bytes!!!!", "sixteen "])))
        .expect("the request builds");
    let (status, _, body) = send(app, request).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(
        Some(ferrohealth_server::refusal::TOO_LARGE),
        body["reason"].as_str()
    );
}

fn problems() -> StackPolicy {
    StackPolicy {
        panic_body: ferrohealth_server::problem::panic_body(),
        refusal_body: ferrohealth_server::problem::refusal_body(),
        ..StackPolicy::default()
    }
}

/// The status, the content type and the JSON body of `request`.
async fn problem_of(
    app: Router,
    request: http::Request<Body>,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let (status, headers, body) = send(app, request).await;
    let content_type = headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (
        status,
        content_type,
        serde_json::from_str(&body).expect("JSON"),
    )
}

#[tokio::test(start_paused = true)]
async fn an_opted_in_timeout_is_problem_json_to_a_client_that_asks_for_json() {
    let app = app(product(), serving(), &section(), problems());
    let request = http::Request::builder()
        .uri("/ferrosys/slow")
        .header(http::header::ACCEPT, "application/json")
        .body(Body::empty())
        .expect("the request builds");
    let (status, content_type, body) = problem_of(app, request).await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
    assert_eq!(
        Some(ferrohealth_server::problem::MEDIA_TYPE),
        content_type.as_deref()
    );
    assert_eq!("about:blank", body["type"]);
    assert_eq!("Request Timeout", body["title"]);
    assert_eq!(408, body["status"]);
    assert_eq!(ferrohealth_server::refusal::TIMEOUT, body["detail"]);
    assert!(
        body["instance"]
            .as_str()
            .is_some_and(|instance| instance.starts_with("urn:uuid:")),
        "{body}"
    );
}

#[tokio::test]
async fn opted_in_body_limits_are_problems_however_the_body_arrives() {
    let section = ServerSection {
        max_concurrent_requests: NonZeroUsize::new(1),
        retry_after_s: 2,
        ..section()
    };
    let app = app(product(), serving(), &section, problems());
    let declared = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .header(http::header::CONTENT_LENGTH, "33")
        .body(Body::from("a body of more than sixteen bytes"))
        .expect("the request builds");
    let (status, content_type, body) = problem_of(app.clone(), declared).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    assert_eq!(
        Some(ferrohealth_server::problem::MEDIA_TYPE),
        content_type.as_deref()
    );
    assert_eq!(413, body["status"]);
    assert_eq!(ferrohealth_server::refusal::TOO_LARGE, body["detail"]);
    let streamed = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo")
        .body(Body::new(Chunks(vec!["bytes!!!!", "sixteen "])))
        .expect("the request builds");
    let (status, _, body) = problem_of(app, streamed).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    assert_eq!(ferrohealth_server::refusal::TOO_LARGE, body["detail"]);
}

#[tokio::test]
async fn an_opted_in_panic_carries_no_message_path_or_trace() {
    let app = app(product(), serving(), &section(), problems());
    let request = http::Request::builder()
        .uri("/ferrosys/boom?patient=Qz7query")
        .body(Body::empty())
        .expect("the request builds");
    let (status, content_type, body) = problem_of(app, request).await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert_eq!(
        Some(ferrohealth_server::problem::MEDIA_TYPE),
        content_type.as_deref()
    );
    assert_eq!(500, body["status"]);
    assert_eq!(ferrohealth_server::problem::INTERNAL, body["detail"]);
    let rendered = body.to_string();
    for leak in ["Qz7", "boom", "panicked", "stack.rs", "SELECT"] {
        assert!(!rendered.contains(leak), "{leak} in {rendered}");
    }
    let members: Vec<_> = body
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        vec!["detail", "instance", "status", "title", "type"],
        members
    );
}

#[tokio::test]
async fn a_product_that_has_not_moved_keeps_its_own_status_and_body() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let (status, headers, _) = get_status(app, "/ferrosys/boom").await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert_ne!(
        Some(ferrohealth_server::problem::MEDIA_TYPE),
        headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    );
}
