// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one stack every server runs around its routes.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use ferrohealth_telemetry::request_log::RequestLog;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};
use tower_http::limit::RequestBodyLimitLayer;

use crate::client_address::Forwarding;
use crate::edge::{self, EdgeLayer};
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
/// From the outside in: the request id, the client address, the refusal
/// rendering, the panic rendering, the panic catch, the request timeout
/// (`408`), the body limit (`413`, and the same ceiling for every
/// extractor), the response
/// compression when it is on, the request log, then the routes, the
/// product's behind the overload bound when one is configured.
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
    let routes = health::routes(health).merge(product);
    let routes = if section.base_path.as_str().is_empty() {
        routes
    } else {
        Router::new().nest(section.base_path.as_str(), routes)
    };
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
        .layer(edge)
        .layer(RequestBodyLimitLayer::new(section.body_limit_bytes))
        .layer(DefaultBodyLimit::max(section.body_limit_bytes));
    if section.compression {
        routes.layer(
            outer
                .layer(
                    CompressionLayer::new().gzip(true).br(true).compress_when(
                        SizeAbove::new(section.compression_min_bytes)
                            .and(NotForContentType::GRPC)
                            .and(NotForContentType::IMAGES)
                            .and(NotForContentType::SSE),
                    ),
                )
                .layer(policy.request_log),
        )
    } else {
        routes.layer(outer.layer(policy.request_log))
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
        let body = axum::body::to_bytes(streamed.into_body(), 4096)
            .await
            .expect("the body");
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body).is_err(),
            "the extractor's own answer is never rewritten"
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
}
