// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one stack every server runs around its routes.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::handler::Handler;
use ferrohealth_telemetry::request_log::RequestLog;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};
use tower_http::limit::RequestBodyLimitLayer;

use crate::client_address::Forwarding;
use crate::edge::{self, EdgeLayer, IdentifyLayer};
use crate::health::{self, Health};
use crate::overload::{self, Admission};
use crate::panic::{self, PanicBody};
use crate::refusal::{self, RefusalBody};
use crate::section::ServerSection;

/// What a product decides inside the stack: the body of a panicked request,
/// the body of a refusal and the request log with its recorders.
#[derive(Clone)]
pub struct StackPolicy {
    /// The body of a panicked request.
    pub panic_body: PanicBody,
    /// The body of a refusal the stack writes: `408`, `413`, `429`, `503`.
    pub refusal_body: RefusalBody,
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
            refusal_body: refusal::default_body(),
            request_log: RequestLog::default(),
        }
    }
}

/// Wraps `product` and the health routes in the stack, under the base path.
///
/// From the outside in: the request id, the request log, the client
/// address, the refusal rendering, the panic rendering, the panic catch,
/// the request timeout (`408`), the body limit (`413`, and the same ceiling
/// for every extractor), the response compression when it is on, then the
/// routes, the product's behind the overload bound when one is configured.
/// Every request leaves one request line, a refusal and a panic included.
///
/// It is [`Stack::new`] with nothing beside the base path and axum's empty
/// `404` for a request no route matched; [`Stack`] adds both.
pub fn app(
    product: Router,
    health: Health,
    section: &ServerSection,
    policy: StackPolicy,
) -> Router {
    Stack::new(product, health, section, policy).into_router()
}

/// The stack around a product, with the routes it serves beside its base
/// path and the fallback of a request no route matched.
///
/// [`Stack::into_router`] mounts the product and the health routes under
/// the base path, as [`app`] does, the routes of [`Stack::beside`] at the
/// root next to them, and the handler of [`Stack::fallback`] for every
/// request no route matched. Every layer of [`app`] covers all three, and
/// the overload bound covers the product's routes and the routes beside.
///
/// # Examples
///
/// ```
/// use axum::Router;
/// use axum::routing::get;
/// use ferrohealth_health::lifecycle::Lifecycle;
/// use ferrohealth_health::registry::Registry;
/// use ferrohealth_server::health::Health;
/// use ferrohealth_server::section::{BasePath, ServerSection};
/// use ferrohealth_server::stack::{Stack, StackPolicy};
/// use http::StatusCode;
///
/// let section = ServerSection {
///     base_path: BasePath::new("/ehr")?,
///     ..ServerSection::default()
/// };
/// let health = Health::new(Registry::default(), Lifecycle::default());
/// // `/ehr/v1/ehr` and `/ehr/health` under the base path, `/fhir/metadata`
/// // beside it, and the product's own `404` for every other path.
/// let product = Router::new().route("/v1/ehr", get(|| async { "ehr" }));
/// let fhir = Router::new().route("/fhir/metadata", get(|| async { "metadata" }));
/// let app: Router = Stack::new(product, health, &section, StackPolicy::default())
///     .beside(fhir)
///     .fallback(|| async { (StatusCode::NOT_FOUND, "no such resource") })
///     .into_router();
/// # let _ = app;
/// # Ok::<(), ferrohealth_server::section::BasePathError>(())
/// ```
#[derive(Debug)]
#[must_use]
pub struct Stack {
    product: Router,
    health: Health,
    section: ServerSection,
    policy: StackPolicy,
    beside: Router,
    fallback: Option<Router>,
}

impl Stack {
    /// The stack around `product` and `health` under `section`, with
    /// `policy`: what [`app`] builds.
    pub fn new(
        product: Router,
        health: Health,
        section: &ServerSection,
        policy: StackPolicy,
    ) -> Self {
        Self {
            product,
            health,
            section: section.clone(),
            policy,
            beside: Router::new(),
            fallback: None,
        }
    }

    /// Mounts `routes` at the root, beside the base path, with every layer
    /// of the stack and the overload bound around them.
    ///
    /// A path of `routes` is served as written, whatever the base path; with
    /// an empty base path they sit next to the product's routes. Called more
    /// than once, the routes add up.
    pub fn beside(mut self, routes: Router) -> Self {
        self.beside = self.beside.merge(routes);
        self
    }

    /// Answers every request no route matched with `handler`, inside every
    /// layer of the stack and outside the overload bound.
    ///
    /// It answers a path outside the base path and every route beside it,
    /// and a path under the base path that no route matched, unless the
    /// product's router has a fallback of its own (axum `Router::nest`,
    /// "Fallbacks"). A second call replaces the first.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.fallback = Some(Router::new().fallback(handler));
        self
    }

    /// Builds the router the listener serves.
    ///
    /// # Panics
    ///
    /// When axum refuses the routes it merges: a route beside the base path
    /// that is also a route under it, or, with [`Stack::fallback`] set, a
    /// router beside or a product under an empty base path that carries a
    /// fallback of its own (axum `Router::merge`, "Panics").
    pub fn into_router(self) -> Router {
        let Self {
            product,
            health,
            section,
            policy,
            beside,
            fallback,
        } = self;
        let admission = section.max_concurrent_requests.map(|limit| {
            Admission::new(limit, std::time::Duration::from_secs(section.retry_after_s))
        });
        let product = match &admission {
            Some(admission) => product.route_layer(axum::middleware::from_fn_with_state(
                admission.clone(),
                overload::admit,
            )),
            None => product,
        };
        let routes = health::routes(health).merge(product);
        let routes = if section.base_path.as_str().is_empty() {
            routes
        } else {
            Router::new().nest(section.base_path.as_str(), routes)
        };
        let beside = match admission {
            Some(admission) if beside.has_routes() => beside.route_layer(
                axum::middleware::from_fn_with_state(admission, overload::admit),
            ),
            _ => beside,
        };
        let routes = routes.merge(beside);
        let routes = match fallback {
            Some(fallback) => routes.merge(fallback),
            None => routes,
        };
        wrap(routes, &section, policy)
    }
}

/// Wraps `routes` in every layer of the stack.
fn wrap(routes: Router, section: &ServerSection, policy: StackPolicy) -> Router {
    // NOTE: axum `Router::layer` wraps every route and the fallback, so the matched
    // route is still known and a request no route matched is logged too.
    let edge = EdgeLayer::new(edge::Settings {
        forwarding: Forwarding::from_section(section),
        panic_body: policy.panic_body,
        refusal_body: policy.refusal_body,
        timeout: section.request_timeout(),
        body_limit: section.body_limit_bytes,
    });
    let outer = ServiceBuilder::new()
        .layer(IdentifyLayer)
        .layer(policy.request_log)
        .layer(edge)
        .layer(RequestBodyLimitLayer::new(section.body_limit_bytes))
        .layer(DefaultBodyLimit::max(section.body_limit_bytes));
    if section.compression {
        routes.layer(
            outer.layer(
                CompressionLayer::new().gzip(true).br(true).compress_when(
                    SizeAbove::new(section.compression_min_bytes)
                        .and(NotForContentType::GRPC)
                        .and(NotForContentType::IMAGES)
                        .and(NotForContentType::SSE),
                ),
            ),
        )
    } else {
        routes.layer(outer)
    }
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

    use super::{Stack, StackPolicy, app};
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

    fn refusing() -> StackPolicy {
        StackPolicy {
            refusal_body: Arc::new(
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

    #[tokio::test(start_paused = true)]
    async fn a_timeout_is_written_with_the_product_refusal_body() {
        let app = app(product(), serving(), &section(), refusing());
        let (status, _, body) = get_status(app, "/ferrosys/slow").await;
        assert_eq!(StatusCode::REQUEST_TIMEOUT, status);
        let body: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(
            serde_json::json!({
                "refused": 408,
                "reason": crate::refusal::TIMEOUT,
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
            retry_after_s: 2,
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
        assert_eq!(crate::refusal::TOO_LARGE, body["reason"]);
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
            (Some(413), Some(crate::refusal::TOO_LARGE)),
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
                        axum::extract::Extension<crate::client_address::ClientAddress>,
                    >| async move {
                        client.map_or_else(
                            || "none".to_owned(),
                            |axum::extract::Extension(crate::client_address::ClientAddress(a))| {
                                a.to_string()
                            },
                        )
                    },
                ),
            )
    }

    async fn send(
        app: Router,
        request: http::Request<Body>,
    ) -> (StatusCode, http::HeaderMap, String) {
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
            forwarded_header: crate::client_address::ForwardedHeader::XForwardedFor,
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
            refusal_body: Arc::new(
                |_uri: &http::Uri, _request_id: &str, status: StatusCode, reason: &str| {
                    let chosen = if status == StatusCode::REQUEST_TIMEOUT {
                        StatusCode::GATEWAY_TIMEOUT
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    };
                    crate::refusal::keep_status(
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
            refusal_body: Arc::new(
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
        assert_ne!(crate::refusal::TOO_LARGE, body, "the extractor's own text");
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
        assert_eq!(Some(crate::refusal::TOO_LARGE), body["reason"].as_str());
    }

    fn problems() -> StackPolicy {
        StackPolicy {
            panic_body: crate::problem::panic_body(),
            refusal_body: crate::problem::refusal_body(),
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
        assert_eq!(Some(crate::problem::MEDIA_TYPE), content_type.as_deref());
        assert_eq!("about:blank", body["type"]);
        assert_eq!("Request Timeout", body["title"]);
        assert_eq!(408, body["status"]);
        assert_eq!(crate::refusal::TIMEOUT, body["detail"]);
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
        assert_eq!(Some(crate::problem::MEDIA_TYPE), content_type.as_deref());
        assert_eq!(413, body["status"]);
        assert_eq!(crate::refusal::TOO_LARGE, body["detail"]);
        let streamed = http::Request::builder()
            .method("POST")
            .uri("/ferrosys/echo")
            .body(Body::new(Chunks(vec!["bytes!!!!", "sixteen "])))
            .expect("the request builds");
        let (status, _, body) = problem_of(app, streamed).await;
        assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status);
        assert_eq!(crate::refusal::TOO_LARGE, body["detail"]);
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
        assert_eq!(Some(crate::problem::MEDIA_TYPE), content_type.as_deref());
        assert_eq!(500, body["status"]);
        assert_eq!(crate::problem::INTERNAL, body["detail"]);
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
            Some(crate::problem::MEDIA_TYPE),
            headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
        );
    }
}
