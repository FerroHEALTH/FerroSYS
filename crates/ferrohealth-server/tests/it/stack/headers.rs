// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The API security headers: on every answer the stack serves or writes,
//! below a route's own, never on an exempt tree, and HSTS only over TLS.

use std::num::NonZeroUsize;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::routing::{get, post};
use http::StatusCode;

use ferrohealth_server::api_headers;
use ferrohealth_server::section::{ServerSection, TlsSection};
use ferrohealth_server::stack::{Stack, StackPolicy, app};

use super::{get_status, product, section, send, serving};

/// The set the stack writes on a plain HTTP listener.
const SET: [(&str, &str); 6] = [
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    ("cross-origin-resource-policy", "same-site"),
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

fn assert_set(headers: &http::HeaderMap, context: &str) {
    for (name, value) in SET {
        assert_eq!(
            Some(value),
            headers.get(name).and_then(|v| v.to_str().ok()),
            "{context}: {name}"
        );
    }
}

fn assert_none(headers: &http::HeaderMap, context: &str) {
    for (name, _) in SET {
        assert!(!headers.contains_key(name), "{context}: {name}");
    }
}

fn oversized(uri: &str) -> http::Request<Body> {
    http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-length", "64")
        .body(Body::from(vec![b'x'; 64]))
        .expect("the request builds")
}

#[tokio::test(start_paused = true)]
async fn every_answer_carries_the_set_and_no_hsts_without_tls() {
    let app = Stack::new(product(), serving(), &section(), StackPolicy::default())
        .fallback(|| async { (StatusCode::NOT_FOUND, "none") })
        .into_router();
    for (uri, status) in [
        ("/ferrosys/fhir/Patient/7", StatusCode::OK),
        ("/ferrosys/health/readiness", StatusCode::OK),
        ("/ferrosys/boom", StatusCode::INTERNAL_SERVER_ERROR),
        ("/ferrosys/slow", StatusCode::REQUEST_TIMEOUT),
        ("/elsewhere", StatusCode::NOT_FOUND),
    ] {
        let (got, headers, _) = get_status(app.clone(), uri).await;
        assert_eq!(status, got, "{uri}");
        assert_set(&headers, uri);
        assert!(!headers.contains_key("strict-transport-security"), "{uri}");
    }
    let (status, headers, _) = send(app, oversized("/ferrosys/echo")).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    assert_set(&headers, "the 413");
}

#[tokio::test]
async fn the_overload_refusal_carries_the_set() {
    let section = ServerSection {
        max_concurrent_requests: NonZeroUsize::new(1),
        ..section()
    };
    let app = app(product(), serving(), &section, StackPolicy::default());
    let held = tokio::spawn(get_status(app.clone(), "/ferrosys/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, headers, _) = get_status(app, "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_set(&headers, "the 503");
    held.await.expect("the held request ends");
}

#[tokio::test]
async fn a_header_the_route_set_is_kept() {
    let page = Router::new().route(
        "/page",
        get(|| async {
            (
                [
                    ("content-security-policy", "script-src 'self'"),
                    ("cache-control", "max-age=60"),
                ],
                "<!doctype html>",
            )
        }),
    );
    let app = app(page, serving(), &section(), StackPolicy::default());
    let (_, headers, _) = get_status(app, "/ferrosys/page").await;
    assert_eq!(
        Some("script-src 'self'"),
        headers
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
    );
    assert_eq!(
        Some("max-age=60"),
        headers.get("cache-control").and_then(|v| v.to_str().ok())
    );
    assert_eq!(
        Some("DENY"),
        headers.get("x-frame-options").and_then(|v| v.to_str().ok()),
        "the headers the route left unset are filled"
    );
}

#[tokio::test]
async fn an_exempt_tree_gets_none_and_the_stack_refusal_in_front_of_it_does() {
    let site = Router::new()
        .route("/site", get(|| async { "<!doctype html>" }))
        .route("/site/form", post(|body: String| async move { body }))
        .layer(axum::middleware::map_response(api_headers::exempt));
    let app = Stack::new(product(), serving(), &section(), StackPolicy::default())
        .beside(site)
        .into_router();
    let (status, headers, _) = get_status(app.clone(), "/site").await;
    assert_eq!(StatusCode::OK, status);
    assert_none(&headers, "the exempt tree");
    let (_, headers, _) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
    assert_set(&headers, "a route outside the exempt tree");
    let (status, headers, _) = send(app, oversized("/site/form")).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
    assert_set(&headers, "the 413 the stack wrote before the tree");
}

#[tokio::test]
async fn a_listener_that_serves_tls_sends_hsts() {
    let section = ServerSection {
        tls: Some(TlsSection::new("cert.pem".into(), "key.pem".into())),
        ..section()
    };
    let app = app(product(), serving(), &section, StackPolicy::default());
    for uri in ["/ferrosys/fhir/Patient/7", "/ferrosys/health/liveness"] {
        let (_, headers, _) = get_status(app.clone(), uri).await;
        assert_eq!(
            Some("max-age=31536000"),
            headers
                .get("strict-transport-security")
                .and_then(|v| v.to_str().ok()),
            "{uri}"
        );
        assert_set(&headers, uri);
    }
}

#[cfg(feature = "console")]
#[tokio::test]
async fn a_console_keeps_every_header_its_middleware_sets() {
    use ferrohealth_server::console::{self, ConsoleHeaders};

    let headers_of = ConsoleHeaders::server_rendered("'self'").with_bundle("/pkg");
    let pages = Router::new()
        .route("/home", get(|| async { "<!doctype html>" }))
        .route("/pkg/app.js", get(|| async { "export {};" }))
        .layer(axum::middleware::from_fn(move |request, next| {
            console::headers(headers_of.clone(), request, next)
        }));
    let app = app(pages, serving(), &section(), StackPolicy::default());
    let (_, page, _) = get_status(app.clone(), "/ferrosys/home").await;
    let value = |map: &http::HeaderMap, name: &str| {
        map.get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(
        Some("no-referrer".to_owned()),
        value(&page, "referrer-policy")
    );
    let csp = value(&page, "content-security-policy").unwrap_or_default();
    assert!(csp.contains("script-src 'self'"), "{csp}");
    let (_, bundle, _) = get_status(app, "/ferrosys/pkg/app.js").await;
    assert_eq!(Some("no-cache".to_owned()), value(&bundle, "cache-control"));
}
