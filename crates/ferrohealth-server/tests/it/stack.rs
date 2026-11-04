// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The stack: the product, the section, the capture and the request
//! helpers every test of it shares.

mod lines;
mod refusals;
mod routes;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use http::StatusCode;
use tower::ServiceExt as _;

use ferrohealth_server::health::Health;
use ferrohealth_server::section::{BasePath, ServerSection};
use ferrohealth_server::stack::StackPolicy;

mod tracing_subscriber_shim {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub(super) struct Capture(pub(super) Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("the buffer").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
}

async fn boom() -> &'static str {
    panic!("patient Qz7sentinel has no EHR")
}

fn product() -> Router {
    Router::new()
        .route("/fhir/Patient/{id}", get(|| async { "ok" }))
        .route("/boom", get(boom))
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                "late"
            }),
        )
        .route(
            "/echo",
            post(|body: String| async move { body.len().to_string() }),
        )
        .route(
            "/hold",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                "held"
            }),
        )
}

fn section() -> ServerSection {
    ServerSection {
        base_path: BasePath::new("/ferrosys").expect("a base path"),
        request_timeout_ms: 1_000,
        body_limit_bytes: 16,
        ..ServerSection::default()
    }
}

fn serving() -> Health {
    let lifecycle = Lifecycle::default();
    lifecycle.booted();
    Health::new(Registry::default(), lifecycle)
}

async fn get_status(app: Router, uri: &str) -> (StatusCode, http::HeaderMap, String) {
    let response = app
        .oneshot(
            http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("the request builds"),
        )
        .await
        .expect("the request is served");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("the body");
    (
        status,
        headers,
        String::from_utf8(body.to_vec()).expect("UTF-8"),
    )
}

fn refusing() -> StackPolicy {
    StackPolicy {
        refusal_body: ferrohealth_server::refusal::from_server_id(
            |uri: &http::Uri, request_id: &str, status: StatusCode, reason: &str| {
                axum::Json(serde_json::json!({
                    "refused": status.as_u16(),
                    "reason": reason,
                    "path": uri.path(),
                    "request_id_length": request_id.len(),
                }))
                .into_response()
            },
        ),
        ..StackPolicy::default()
    }
}

async fn late_boom() -> &'static str {
    tokio::time::sleep(Duration::from_millis(5)).await;
    panic!("patient Qz7sentinel left")
}

fn waiting() -> Router {
    Router::new()
        .route("/late-boom", get(late_boom))
        .route(
            "/brief",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                "brief"
            }),
        )
        .route(
            "/own-408",
            get(|| async { (StatusCode::REQUEST_TIMEOUT, "the handler's own") }),
        )
        .route(
            "/own-413",
            get(|| async { (StatusCode::PAYLOAD_TOO_LARGE, "the handler's own") }),
        )
        .route(
            "/client",
            get(
                |client: Option<
                    axum::extract::Extension<ferrohealth_server::client_address::ClientAddress>,
                >| async move {
                    client.map_or_else(
                        || "none".to_owned(),
                        |axum::extract::Extension(
                            ferrohealth_server::client_address::ClientAddress(a),
                        )| { a.to_string() },
                    )
                },
            ),
        )
}

async fn send(app: Router, request: http::Request<Body>) -> (StatusCode, http::HeaderMap, String) {
    let response = app.oneshot(request).await.expect("the request is served");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("the body");
    (
        status,
        headers,
        String::from_utf8(body.to_vec()).expect("UTF-8"),
    )
}
