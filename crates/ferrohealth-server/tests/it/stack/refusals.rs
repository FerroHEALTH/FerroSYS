// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The refusals and the panics: the timeout, the body limit, the bound, the
//! panic catch and the ids every answer names.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse as _;
use axum::routing::get;
use http::StatusCode;
use tower::ServiceExt as _;

use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{StackPolicy, app};

use super::tracing_subscriber_shim::Capture;
use super::{get_status, product, refusing, section, send, serving, waiting};

#[tokio::test]
async fn every_request_gets_one_line_and_a_404_names_no_route() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_writer(capture.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let app = app(product(), serving(), &section(), StackPolicy::default());
    for uri in [
        "/ferrosys/fhir/Patient/7",
        "/ferrosys/nothing",
        "/elsewhere",
    ] {
        let _answered = get_status(app.clone(), uri).await;
    }
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .filter(|line: &serde_json::Value| line["message"] == "request")
        .collect();
    let seen: Vec<(String, u64)> = lines
        .iter()
        .map(|line| {
            (
                line["route"].as_str().unwrap_or_default().to_owned(),
                line["status"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            ("/ferrosys/fhir/Patient/{id}".to_owned(), 200),
            ("<unmatched>".to_owned(), 404),
            ("<unmatched>".to_owned(), 404),
        ],
        seen
    );
}

#[tokio::test]
async fn a_panic_is_a_500_with_the_product_body_and_no_message_anywhere() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(capture.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let seen = Arc::new(Mutex::new(String::new()));
    let recorded = Arc::clone(&seen);
    let policy = StackPolicy {
        panic_body: ferrohealth_server::panic::from_server_id(
            move |uri: &http::Uri, request_id: &str| {
                *recorded.lock().expect("the slot") = request_id.to_owned();
                format!(
                    "{{\"resourceType\":\"OperationOutcome\",\"path\":\"{}\"}}",
                    uri.path()
                )
                .into_response()
            },
        ),
        ..StackPolicy::default()
    };
    let app = app(product(), serving(), &section(), policy);
    let (status, headers, body) = get_status(app, "/ferrosys/boom").await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert_eq!(
        r#"{"resourceType":"OperationOutcome","path":"/ferrosys/boom"}"#,
        body
    );
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    assert!(
        !log.contains("Qz7sentinel") && !body.contains("Qz7sentinel"),
        "the message is withheld: {log}"
    );
    assert!(log.contains("the request handler panicked"), "{log}");
    let server_id = seen.lock().expect("the slot").clone();
    assert!(
        log.contains(&server_id),
        "the line names the server's request id"
    );
    assert_eq!(
        Some(server_id.as_str()),
        headers.get("x-request-id").and_then(|v| v.to_str().ok())
    );
}

#[tokio::test]
async fn the_default_panic_body_names_the_request_id() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let (status, _, body) = get_status(app, "/ferrosys/boom").await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!("internal", body["error"]);
    assert_eq!(36, body["request_id"].as_str().map_or(0, str::len));
}

#[tokio::test(start_paused = true)]
async fn a_slow_request_is_answered_408() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let (status, _, _) = get_status(app, "/ferrosys/slow").await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
}

#[tokio::test]
async fn a_body_past_the_limit_is_answered_413() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    for (body, status) in [
        ("short", StatusCode::OK),
        (
            "a body of more than sixteen bytes",
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .method("POST")
                    .uri("/ferrosys/echo")
                    .body(Body::from(body))
                    .expect("the request builds"),
            )
            .await
            .expect("the request is served");
        assert_eq!(status, response.status(), "{body}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_timeout_is_written_with_the_product_refusal_body() {
    let app = app(product(), serving(), &section(), refusing());
    let (status, _, body) = get_status(app, "/ferrosys/slow").await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(
        serde_json::json!({
            "refused": 408,
            "reason": ferrohealth_server::refusal::TIMEOUT,
            "path": "/ferrosys/slow",
            "request_id_length": 36,
        }),
        body
    );
}

#[tokio::test]
async fn the_limit_and_the_bound_are_written_with_the_product_refusal_body() {
    let section = ServerSection {
        max_concurrent_requests: NonZeroUsize::new(1),
        retry_after: ferrohealth_config::units::Duration::from_secs(2),
        ..section()
    };
    let app = app(product(), serving(), &section, refusing());
    let declared = app
        .clone()
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri("/ferrosys/echo")
                .header(http::header::CONTENT_LENGTH, "33")
                .body(Body::from("a body of more than sixteen bytes"))
                .expect("the request builds"),
        )
        .await
        .expect("the request is served");
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, declared.status());
    let body = axum::body::to_bytes(declared.into_body(), 4096)
        .await
        .expect("the body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON");
    assert_eq!(ferrohealth_server::refusal::TOO_LARGE, body["reason"]);
    let streamed = app
        .clone()
        .oneshot(
            http::Request::builder()
                .method("POST")
                .uri("/ferrosys/echo")
                .body(Body::from("a body of more than sixteen bytes"))
                .expect("the request builds"),
        )
        .await
        .expect("the request is served");
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, streamed.status());
    assert!(
        streamed.headers().contains_key("x-request-id"),
        "the streamed refusal echoes the request id"
    );
    let body = axum::body::to_bytes(streamed.into_body(), 4096)
        .await
        .expect("the body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON");
    assert_eq!(
        (Some(413), Some(ferrohealth_server::refusal::TOO_LARGE)),
        (body["refused"].as_u64(), body["reason"].as_str()),
        "the extractor's 413 is written with the product's body"
    );
    let held = tokio::spawn(get_status(app.clone(), "/ferrosys/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, headers, body) = get_status(app, "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(
        Some("2"),
        headers.get("retry-after").and_then(|v| v.to_str().ok()),
        "the refusal keeps its Retry-After"
    );
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(503, body["refused"]);
    let _held = held.await.expect("the held request ends");
}

#[tokio::test]
async fn past_the_bound_a_request_is_refused_and_health_is_never_refused() {
    let section = ServerSection {
        max_concurrent_requests: NonZeroUsize::new(1),
        retry_after: ferrohealth_config::units::Duration::from_secs(2),
        ..section()
    };
    let app = app(product(), serving(), &section, StackPolicy::default());
    let held = tokio::spawn(get_status(app.clone(), "/ferrosys/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, headers, _) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(
        Some("2"),
        headers.get("retry-after").and_then(|v| v.to_str().ok())
    );
    let (status, _, _) = get_status(app.clone(), "/ferrosys/health/readiness").await;
    assert_eq!(StatusCode::OK, status, "the health family passes the bound");
    let (status, _, body) = held.await.expect("the held request ends");
    assert_eq!((StatusCode::OK, "held"), (status, body.as_str()));
    let (status, _, _) = get_status(app, "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::OK, status, "the slot is free again");
}

#[tokio::test(start_paused = true)]
async fn every_answer_the_stack_writes_echoes_the_request_id() {
    let app = app(
        product().merge(waiting()),
        serving(),
        &section(),
        refusing(),
    );
    let requests = [
        ("GET", "/ferrosys/slow", None, StatusCode::REQUEST_TIMEOUT),
        (
            "GET",
            "/ferrosys/boom",
            None,
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "GET",
            "/ferrosys/late-boom",
            None,
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "POST",
            "/ferrosys/echo",
            Some("33"),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        ("GET", "/ferrosys/brief", None, StatusCode::OK),
    ];
    for (method, uri, length, status) in requests {
        for client in [None, Some("corr-42")] {
            let mut request = http::Request::builder().method(method).uri(uri);
            if let Some(length) = length {
                request = request.header(http::header::CONTENT_LENGTH, length);
            }
            if let Some(client) = client {
                request = request.header("x-request-id", client);
            }
            let body = if length.is_some() {
                Body::from("a body of more than sixteen bytes")
            } else {
                Body::empty()
            };
            let (got, headers, _) =
                send(app.clone(), request.body(body).expect("the request builds")).await;
            assert_eq!(status, got, "{uri}");
            let echoed = headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            match client {
                Some(client) => assert_eq!(client, echoed, "{uri} echoes the client's id"),
                None => assert_eq!(36, echoed.len(), "{uri} echoes the server's id"),
            }
        }
    }
}

/// A policy whose bodies write both ids, the panic's as `500` and each
/// refusal's with its status.
fn naming_both_ids() -> StackPolicy {
    StackPolicy {
        panic_body: Arc::new(
            |_uri: &http::Uri, ids: ferrohealth_server::request_id::RequestIds<'_>| {
                axum::Json(serde_json::json!({"server": ids.server, "echoed": ids.echoed}))
                    .into_response()
            },
        ),
        refusal_body: Arc::new(
            |_uri: &http::Uri,
             ids: ferrohealth_server::request_id::RequestIds<'_>,
             _status: StatusCode,
             _reason: &str| {
                axum::Json(serde_json::json!({"server": ids.server, "echoed": ids.echoed}))
                    .into_response()
            },
        ),
        ..StackPolicy::default()
    }
}

/// The ids the body named and the id the response echoed, for `GET uri`
/// with the client id `client`.
async fn named_ids(
    app: Router,
    uri: &str,
    client: Option<&str>,
) -> (StatusCode, String, String, String) {
    let mut request = http::Request::builder().uri(uri);
    if let Some(client) = client {
        request = request.header("x-request-id", client);
    }
    let (status, headers, body) = send(
        app,
        request.body(Body::empty()).expect("the request builds"),
    )
    .await;
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    (
        status,
        body["server"].as_str().unwrap_or_default().to_owned(),
        body["echoed"].as_str().unwrap_or_default().to_owned(),
        headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned(),
    )
}

#[tokio::test]
async fn a_panic_body_names_the_id_the_response_echoes_beside_the_server_s() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(capture.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let app = app(product(), serving(), &section(), naming_both_ids());
    let (status, server, echoed, header) =
        named_ids(app.clone(), "/ferrosys/boom", Some("corr-panic")).await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert_eq!(
        ("corr-panic", "corr-panic"),
        (echoed.as_str(), header.as_str())
    );
    assert_eq!(36, server.len(), "the server's id stays available");
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    assert!(
        log.contains(&server),
        "the panic line names the server's id: {log}"
    );
    assert!(
        !log.contains("corr-panic"),
        "the client's id is never logged: {log}"
    );
    let (status, server, echoed, header) = named_ids(app, "/ferrosys/boom", None).await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert_eq!(36, server.len());
    assert_eq!(
        (server.as_str(), server.as_str()),
        (echoed.as_str(), header.as_str())
    );
}

#[tokio::test(start_paused = true)]
async fn a_refusal_body_names_the_id_the_response_echoes_beside_the_server_s() {
    let app = app(product(), serving(), &section(), naming_both_ids());
    let (status, server, echoed, header) =
        named_ids(app.clone(), "/ferrosys/slow", Some("corr-408")).await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
    assert_eq!(("corr-408", "corr-408"), (echoed.as_str(), header.as_str()));
    assert_eq!(36, server.len(), "the server's id stays available");
    let (status, server, echoed, header) = named_ids(app, "/ferrosys/slow", None).await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
    assert_eq!(36, server.len());
    assert_eq!(
        (server.as_str(), server.as_str()),
        (echoed.as_str(), header.as_str())
    );
}

#[tokio::test]
async fn a_panic_after_the_handler_waited_is_caught() {
    let app = app(waiting(), serving(), &section(), StackPolicy::default());
    let (status, _, body) = get_status(app, "/ferrosys/late-boom").await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert!(
        !body.contains("Qz7sentinel"),
        "the message is withheld: {body}"
    );
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!("internal", body["error"]);
}

#[tokio::test]
async fn a_handler_that_waits_inside_the_timeout_is_answered() {
    let app = app(waiting(), serving(), &section(), StackPolicy::default());
    let (status, _, body) = get_status(app, "/ferrosys/brief").await;
    assert_eq!((StatusCode::OK, "brief"), (status, body.as_str()));
}

#[tokio::test]
async fn a_408_or_413_the_handler_wrote_is_never_rewritten() {
    let app = app(waiting(), serving(), &section(), refusing());
    for uri in ["/ferrosys/own-408", "/ferrosys/own-413"] {
        let (_, _, body) = get_status(app.clone(), uri).await;
        assert_eq!("the handler's own", body, "{uri}");
    }
}

#[tokio::test]
async fn the_client_address_reaches_the_handler_through_the_stack() {
    let section = ServerSection {
        trusted_proxies: vec!["127.0.0.1".parse().expect("a proxy")],
        forwarded_header: ferrohealth_server::client_address::ForwardedHeader::XForwardedFor,
        ..section()
    };
    let app = app(waiting(), serving(), &section, StackPolicy::default());
    let mut request = http::Request::builder()
        .uri("/ferrosys/client")
        .header("x-forwarded-for", "198.51.100.9")
        .body(Body::empty())
        .expect("the request builds");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40_000,
        ))));
    let (_, _, body) = send(app.clone(), request).await;
    assert_eq!("198.51.100.9", body);
    let (_, _, body) = get_status(app, "/ferrosys/client").await;
    assert_eq!("none", body, "a request over no connection names none");
}

#[tokio::test]
async fn the_line_names_the_id_the_response_echoes() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_writer(capture.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let (_, headers, _) = get_status(app, "/ferrosys/fhir/Patient/7").await;
    let echoed = headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    let line: serde_json::Value = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .find(|line: &serde_json::Value| line["message"] == "request")
        .expect("one request line");
    assert_eq!(36, echoed.len(), "{echoed}");
    assert_eq!(Some(echoed.as_str()), line["request_id"].as_str());
    assert_eq!(Some(false), line["client_named"].as_bool());
}

async fn encoding(app: Router, uri: &str, accept: Option<&str>) -> Option<String> {
    let mut request = http::Request::builder().uri(uri);
    if let Some(accept) = accept {
        request = request.header(http::header::ACCEPT_ENCODING, accept);
    }
    let response = app
        .oneshot(request.body(Body::empty()).expect("the request builds"))
        .await
        .expect("the request is served");
    response
        .headers()
        .get(http::header::CONTENT_ENCODING)
        .map(|value| value.to_str().expect("ASCII").to_owned())
}

#[tokio::test]
async fn a_large_answer_is_compressed_as_the_client_accepts() {
    let product = || {
        Router::new()
            .route("/large", get(|| async { "a".repeat(4096) }))
            .route("/small", get(|| async { "a" }))
    };
    let on = app(
        product(),
        serving(),
        &ServerSection::default(),
        StackPolicy::default(),
    );
    assert_eq!(
        Some("gzip".to_owned()),
        encoding(on.clone(), "/large", Some("gzip")).await
    );
    assert_eq!(
        Some("br".to_owned()),
        encoding(on.clone(), "/large", Some("br")).await
    );
    assert_eq!(
        None,
        encoding(on.clone(), "/large", None).await,
        "nothing accepted, nothing compressed"
    );
    assert_eq!(
        None,
        encoding(on, "/small", Some("gzip")).await,
        "below the threshold"
    );
    let off = ServerSection {
        compression: false,
        ..ServerSection::default()
    };
    let off = app(product(), serving(), &off, StackPolicy::default());
    assert_eq!(None, encoding(off, "/large", Some("gzip")).await);
}
