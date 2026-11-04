// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The routes: the base path, the routes beside it, the fallback and the
//! health family, each inside every layer.

use std::num::NonZeroUsize;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse as _;
use axum::routing::get;
use http::StatusCode;
use tower::ServiceExt as _;

use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{Stack, StackPolicy, app};

use super::tracing_subscriber_shim::Capture;
use super::{boom, get_status, product, refusing, section, serving};

#[tokio::test]
async fn the_routes_and_the_health_family_are_served_under_the_base_path() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    for (uri, status) in [
        ("/ferrosys/fhir/Patient/7", StatusCode::OK),
        ("/ferrosys/health", StatusCode::OK),
        ("/ferrosys/health/liveness", StatusCode::OK),
        ("/ferrosys/health/readiness", StatusCode::OK),
        ("/health", StatusCode::NOT_FOUND),
    ] {
        let (got, headers, _) = get_status(app.clone(), uri).await;
        assert_eq!(status, got, "{uri}");
        assert!(
            headers.contains_key("x-request-id"),
            "{uri} carries a request id"
        );
    }
    let (_, _, body) = get_status(app, "/ferrosys/health/readiness").await;
    assert_eq!(r#"{"state":"up","phase":"serving","indicators":[]}"#, body);
}

fn beside_and_fallback(section: &ServerSection, policy: StackPolicy) -> Router {
    Stack::new(product(), serving(), section, policy)
        .beside(
            Router::new()
                .route("/fhir/r4/Patient/{id}", get(|| async { "beside" }))
                .route("/fhir/r4/boom", get(boom))
                .route(
                    "/fhir/r4/hold",
                    get(|| async {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        "held"
                    }),
                ),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, "the product's own 404") })
        .into_router()
}

#[tokio::test]
async fn routes_beside_the_base_path_and_the_fallback_run_inside_the_stack() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_writer(capture.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let section = ServerSection {
        compression_min_bytes: 0,
        ..section()
    };
    let app = beside_and_fallback(&section, StackPolicy::default());
    let mut ids = Vec::new();
    for (uri, status, body) in [
        ("/ferrosys/fhir/Patient/7", StatusCode::OK, "ok"),
        (
            "/ferrosys/health/liveness",
            StatusCode::OK,
            r#"{"state":"up"}"#,
        ),
        ("/fhir/r4/Patient/7", StatusCode::OK, "beside"),
        ("/elsewhere", StatusCode::NOT_FOUND, "the product's own 404"),
        (
            "/ferrosys/nothing",
            StatusCode::NOT_FOUND,
            "the product's own 404",
        ),
        (
            "/ferrosys/fhir/r4/Patient/7",
            StatusCode::NOT_FOUND,
            "the product's own 404",
        ),
        ("/health", StatusCode::NOT_FOUND, "the product's own 404"),
    ] {
        let (got, headers, got_body) = get_status(app.clone(), uri).await;
        assert_eq!((status, body), (got, got_body.as_str()), "{uri}");
        let id = headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        assert!(id.is_some(), "{uri} carries a request id");
        ids.extend(id);
    }
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .filter(|line: &serde_json::Value| line["message"] == "request")
        .collect();
    let seen: Vec<(String, u64, String)> = lines
        .iter()
        .map(|line| {
            (
                line["route"].as_str().unwrap_or_default().to_owned(),
                line["status"].as_u64().unwrap_or_default(),
                line["request_id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let routes = [
        ("/ferrosys/fhir/Patient/{id}", 200),
        ("/ferrosys/health/liveness", 200),
        ("/fhir/r4/Patient/{id}", 200),
        ("<unmatched>", 404),
        ("<unmatched>", 404),
        ("<unmatched>", 404),
        ("<unmatched>", 404),
    ];
    let expected: Vec<(String, u64, String)> = routes
        .iter()
        .zip(&ids)
        .map(|((route, status), id)| ((*route).to_owned(), *status, id.clone()))
        .collect();
    assert_eq!(expected, seen, "one line per request, each with its id");
}

#[tokio::test]
async fn every_layer_covers_the_routes_beside_and_the_fallback() {
    let section = ServerSection {
        compression_min_bytes: 0,
        max_concurrent_requests: NonZeroUsize::new(1),
        retry_after_s: 2,
        ..section()
    };
    let app = beside_and_fallback(&section, refusing());
    let (status, headers, body) = get_status(app.clone(), "/fhir/r4/boom").await;
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, status);
    assert!(!body.contains("Qz7sentinel"), "{body}");
    assert!(headers.contains_key("x-request-id"));
    for uri in ["/fhir/r4/Patient/7", "/elsewhere"] {
        let response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri(uri)
                    .header(http::header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("the request is served");
        assert_eq!(
            Some("gzip"),
            response
                .headers()
                .get(http::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            "{uri} is compressed"
        );
    }
    let held = tokio::spawn(get_status(app.clone(), "/fhir/r4/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, _, body) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
    assert_eq!(
        StatusCode::SERVICE_UNAVAILABLE,
        status,
        "one bound for both"
    );
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(503, body["refused"]);
    let (status, _, _) = get_status(app.clone(), "/elsewhere").await;
    assert_eq!(StatusCode::NOT_FOUND, status, "the fallback is not bounded");
    let (status, _, body) = held.await.expect("the held request ends");
    assert_eq!((StatusCode::OK, "held"), (status, body.as_str()));
}

#[tokio::test(start_paused = true)]
async fn a_slow_route_beside_is_answered_408_with_the_product_body() {
    let app = Stack::new(product(), serving(), &section(), refusing())
        .beside(Router::new().route(
            "/fhir/r4/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                "late"
            }),
        ))
        .into_router();
    let (status, _, body) = get_status(app, "/fhir/r4/slow").await;
    assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
    let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
    assert_eq!(408, body["refused"]);
}

#[tokio::test]
async fn without_a_fallback_a_path_outside_is_axums_empty_404() {
    let app = app(product(), serving(), &section(), StackPolicy::default());
    let (status, headers, body) = get_status(app, "/elsewhere").await;
    assert_eq!((StatusCode::NOT_FOUND, ""), (status, body.as_str()));
    assert!(headers.contains_key("x-request-id"));
}

async fn guard(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if request.headers().contains_key(http::header::AUTHORIZATION) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

fn guarded_product() -> Router {
    product().layer(axum::middleware::from_fn(guard))
}

#[tokio::test]
async fn app_keeps_the_layers_of_the_product_on_a_path_no_route_matched() {
    let app = app(
        guarded_product(),
        serving(),
        &ServerSection::default(),
        StackPolicy::default(),
    );
    for (uri, status) in [
        ("/fhir/Patient/7", StatusCode::UNAUTHORIZED),
        ("/fhir/Practitioner", StatusCode::UNAUTHORIZED),
        ("/nowhere", StatusCode::UNAUTHORIZED),
        ("/health/liveness", StatusCode::OK),
    ] {
        let (got, headers, _) = get_status(app.clone(), uri).await;
        assert_eq!(status, got, "{uri}");
        assert!(headers.contains_key("x-request-id"), "{uri}");
    }
}

#[tokio::test]
async fn routes_beside_with_no_fallback_keep_the_layers_of_the_product_on_a_path_no_route_matched()
{
    let app = Stack::new(
        guarded_product(),
        serving(),
        &ServerSection::default(),
        StackPolicy::default(),
    )
    .beside(Router::new().route("/open", get(|| async { "open" })))
    .into_router();
    for (uri, status) in [
        ("/open", StatusCode::OK),
        ("/fhir/Practitioner", StatusCode::UNAUTHORIZED),
        ("/nowhere", StatusCode::UNAUTHORIZED),
    ] {
        let (got, _, _) = get_status(app.clone(), uri).await;
        assert_eq!(status, got, "{uri}");
    }
}

#[tokio::test]
async fn the_fallback_routes_beside_declare_answers_a_path_no_route_matched() {
    let app = Stack::new(
        guarded_product(),
        serving(),
        &ServerSection::default(),
        StackPolicy::default(),
    )
    .beside(
        Router::new()
            .route("/open", get(|| async { "open" }))
            .fallback(|| async { (StatusCode::NOT_FOUND, "beside's 404") }),
    )
    .into_router();
    let (status, _, body) = get_status(app.clone(), "/nowhere").await;
    assert_eq!(
        (StatusCode::NOT_FOUND, "beside's 404"),
        (status, body.as_str())
    );
    let (status, _, _) = get_status(app, "/fhir/Patient/7").await;
    assert_eq!(StatusCode::UNAUTHORIZED, status);
}

#[tokio::test]
async fn readiness_is_withdrawn_while_draining_and_liveness_holds() {
    let health = serving();
    let lifecycle = health.lifecycle().clone();
    let app = app(product(), health, &section(), StackPolicy::default());
    lifecycle.drain();
    let (status, _, body) = get_status(app.clone(), "/ferrosys/health/readiness").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert!(body.contains(r#""phase":"draining""#), "{body}");
    let (status, _, body) = get_status(app.clone(), "/ferrosys/health").await;
    assert_eq!(
        (StatusCode::SERVICE_UNAVAILABLE, r#"{"state":"down"}"#),
        (status, body.as_str())
    );
    let (status, _, _) = get_status(app, "/ferrosys/health/liveness").await;
    assert_eq!(StatusCode::OK, status);
}
