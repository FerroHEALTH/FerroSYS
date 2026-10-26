// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one stack every server runs around its routes.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use ferrohealth_telemetry::request_log::{self, RequestLog};
use http::StatusCode;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

use crate::health::{self, Health};
use crate::overload::{self, Admission};
use crate::panic::{self, PanicBody};
use crate::request_id;
use crate::section::ServerSection;

/// What a product decides inside the stack: the body of a panicked request
/// and the request log with its recorders.
#[derive(Clone)]
pub struct StackPolicy {
    /// The body of a panicked request.
    pub panic_body: PanicBody,
    /// The request log.
    pub request_log: RequestLog,
}

impl std::fmt::Debug for StackPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackPolicy")
            .field("request_log", &self.request_log)
            .finish_non_exhaustive()
    }
}

impl Default for StackPolicy {
    fn default() -> Self {
        Self {
            panic_body: panic::default_body(),
            request_log: RequestLog::default(),
        }
    }
}

/// Wraps `product` and the health routes in the stack, under the base path.
///
/// From the outside in: the request id, the panic rendering, the panic
/// catch, the request timeout (`408`), the body limit (`413`, and the same
/// ceiling for every extractor), then each route with the request log, and
/// the product's routes behind the overload bound when one is configured.
pub fn app(
    product: Router,
    health: Health,
    section: &ServerSection,
    policy: StackPolicy,
) -> Router {
    let product = match section.max_concurrent_requests {
        Some(limit) => product.route_layer(axum::middleware::from_fn_with_state(
            Admission::new(limit, std::time::Duration::from_secs(section.retry_after_s)),
            overload::admit,
        )),
        None => product,
    };
    let routes =
        health::routes(health)
            .merge(product)
            .route_layer(axum::middleware::from_fn_with_state(
                policy.request_log,
                request_log::middleware,
            ));
    let routes = if section.base_path.as_str().is_empty() {
        routes
    } else {
        Router::new().nest(section.base_path.as_str(), routes)
    };
    let body = policy.panic_body;
    routes
        .layer(DefaultBodyLimit::max(section.body_limit_bytes))
        .layer(RequestBodyLimitLayer::new(section.body_limit_bytes))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            section.request_timeout(),
        ))
        .layer(CatchPanicLayer::custom(panic::caught))
        .layer(axum::middleware::from_fn(move |request, next| {
            panic::render(body.clone(), request, next)
        }))
        .layer(axum::middleware::from_fn(request_id::assign))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::response::IntoResponse as _;
    use axum::routing::{get, post};
    use ferrohealth_health::lifecycle::Lifecycle;
    use ferrohealth_health::registry::Registry;
    use http::StatusCode;
    use tower::ServiceExt as _;
    use tracing_subscriber_shim::Capture;

    use super::{StackPolicy, app};
    use crate::health::Health;
    use crate::section::{BasePath, ServerSection};

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
            panic_body: Arc::new(move |uri: &http::Uri, request_id: &str| {
                *recorded.lock().expect("the slot") = request_id.to_owned();
                format!(
                    "{{\"resourceType\":\"OperationOutcome\",\"path\":\"{}\"}}",
                    uri.path()
                )
                .into_response()
            }),
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

    #[tokio::test]
    async fn past_the_bound_a_request_is_refused_and_health_is_never_refused() {
        let section = ServerSection {
            max_concurrent_requests: NonZeroUsize::new(1),
            retry_after_s: 2,
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
}
