// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The request span and the HTTP server metrics: the trace parent, the span
//! status and the duration and active-request instruments.

use std::sync::Arc;

use crate::request_log::{RequestLog, middleware};
use crate::section::{Format, TelemetrySection};
use crate::telemetry::{Console, Service, Telemetry};
use axum::Router;
use axum::body::Body;
use axum::routing::get;
use opentelemetry::trace::{SpanId, TraceId};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

use super::{Capture, app, get_request, request};

#[tokio::test]
async fn a_client_traceparent_is_never_the_parent_of_the_request_span() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let telemetry = Telemetry::with_tracers(Service::new("ferrosys", "0.0.0"), provider);
    let section = TelemetrySection::default();
    let subscriber = tracing_subscriber::registry().with(
        telemetry
            .layers(&section, &Console::new(false, None), std::io::sink)
            .expect("the layers build")
            .layers,
    );
    let _default = tracing::subscriber::set_default(subscriber);
    let response = app(RequestLog::default())
        .oneshot(request())
        .await
        .expect("the request is served");
    assert_eq!(http::StatusCode::OK, response.status());
    // The simple exporter exports each span as it ends, so no flush is needed.
    let spans = exporter.get_finished_spans().expect("the spans");
    telemetry.shutdown().expect("the providers stop");
    assert_eq!(1, spans.len(), "one span: the request's");
    let span = spans.first().expect("the request span");
    assert_eq!("GET /fhir/Patient/{id}", span.name);
    assert_eq!(opentelemetry::trace::SpanKind::Server, span.span_kind);
    assert_eq!(opentelemetry::trace::Status::Unset, span.status);
    assert_eq!(SpanId::INVALID, span.parent_span_id, "the span is a root");
    assert_ne!(
        TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").expect("the client's trace id"),
        span.span_context.trace_id(),
        "the trace is the server's own"
    );
    let route = span
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "http.route")
        .map(|kv| kv.value.to_string());
    assert_eq!(Some("/fhir/Patient/{id}".to_owned()), route);
    let status = span
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "http.response.status_code")
        .map(|kv| kv.value.to_string());
    assert_eq!(Some("200".to_owned()), status);
}

#[test]
fn an_unknown_method_is_other_everywhere_and_a_server_error_marks_the_span() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let telemetry = Telemetry::with_tracers(Service::new("ferrosys", "0.0.0"), provider);
    let section = TelemetrySection {
        format: Format::Json,
        ..TelemetrySection::default()
    };
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(
        telemetry
            .layers(&section, &Console::new(false, None), capture.clone())
            .expect("the layers build")
            .layers,
    );
    let app = Router::new()
        .route("/v1/query/aql", axum::routing::any(|| async { "ok" }))
        .route("/broken", get(|| async { http::StatusCode::BAD_GATEWAY }))
        .layer(RequestLog::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("the runtime builds");
    tracing::subscriber::with_default(subscriber, || {
        for (method, uri) in [
            ("QZ7METHOD", "/v1/query/aql"),
            ("POST", "/v1/query/aql"),
            ("GET", "/broken"),
            ("GET", "/elsewhere"),
        ] {
            let request = http::Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("the request builds");
            runtime
                .block_on(app.clone().oneshot(request))
                .expect("the request is served");
        }
    });
    let spans = exporter.get_finished_spans().expect("the spans");
    telemetry.shutdown().expect("the providers stop");
    let seen: Vec<(String, String, opentelemetry::trace::Status)> = spans
        .iter()
        .map(|span| {
            let method = span
                .attributes
                .iter()
                .find(|kv| kv.key.as_str() == "http.request.method")
                .map(|kv| kv.value.to_string())
                .unwrap_or_default();
            assert_eq!(opentelemetry::trace::SpanKind::Server, span.span_kind);
            (span.name.to_string(), method, span.status.clone())
        })
        .collect();
    assert_eq!(
        vec![
            (
                "HTTP /v1/query/aql".to_owned(),
                "_OTHER".to_owned(),
                opentelemetry::trace::Status::Unset
            ),
            (
                "POST /v1/query/aql".to_owned(),
                "POST".to_owned(),
                opentelemetry::trace::Status::Unset
            ),
            (
                "GET /broken".to_owned(),
                "GET".to_owned(),
                opentelemetry::trace::Status::error("")
            ),
            (
                "GET".to_owned(),
                "GET".to_owned(),
                opentelemetry::trace::Status::Unset
            ),
        ],
        seen
    );
    let text = capture.text();
    let methods: Vec<String> = text
        .lines()
        .map(|line| {
            let line: serde_json::Value = serde_json::from_str(line).expect("a JSON line");
            line["method"].as_str().unwrap_or_default().to_owned()
        })
        .collect();
    assert_eq!(vec!["_OTHER", "POST", "GET", "GET"], methods);
    assert!(!text.contains("QZ7METHOD"), "{text}");
    assert!(
        !format!("{spans:?}").contains("QZ7METHOD"),
        "no span carries the client's token"
    );
}

#[cfg(feature = "prometheus")]
#[tokio::test]
async fn the_duration_histogram_reaches_the_prometheus_route() {
    let telemetry = Arc::new(
        Telemetry::new(
            &TelemetrySection::default(),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect("the telemetry builds"),
    );
    let log = RequestLog::default()
        .with_metrics(&telemetry.meter("ferrohealth_telemetry"))
        .serving_tls(true);
    let app = Router::new()
        .route("/fhir/Patient/{id}", get(|| async { "{}" }))
        .route(
            "/fail",
            get(|| async { http::StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/metrics",
            crate::telemetry::prometheus_route(Arc::clone(&telemetry)),
        )
        .layer(axum::middleware::from_fn_with_state(log, middleware));
    for uri in ["/fhir/Patient/1", "/fhir/Patient/2", "/fail"] {
        app.clone()
            .oneshot(get_request(uri))
            .await
            .expect("the request is served");
    }
    let response = app
        .oneshot(get_request("/metrics"))
        .await
        .expect("the metrics are served");
    assert_eq!(http::StatusCode::OK, response.status());
    assert_eq!(
        Some("text/plain; version=0.0.4"),
        response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body reads");
    let text = String::from_utf8(body.to_vec()).expect("UTF-8");
    assert!(
        text.contains("# TYPE http_server_request_duration_seconds histogram"),
        "{text}"
    );
    assert!(
        text.contains(
            "http_server_request_duration_seconds_count{http_request_method=\"GET\",http_response_status_code=\"200\",http_route=\"/fhir/Patient/{id}\",network_protocol_version=\"1.1\",url_scheme=\"https\"} 2"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "http_server_request_duration_seconds_count{error_type=\"503\",http_request_method=\"GET\",http_response_status_code=\"503\",http_route=\"/fail\",network_protocol_version=\"1.1\",url_scheme=\"https\"} 1"
        ),
        "{text}"
    );
    assert!(
        text.contains("http_server_request_duration_seconds_bucket{")
            && text.contains("le=\"0.005\""),
        "the advisory buckets: {text}"
    );
}

#[cfg(feature = "prometheus")]
#[tokio::test]
async fn a_request_is_counted_in_while_in_flight_and_out_when_finished_or_dropped() {
    let telemetry = Arc::new(
        Telemetry::new(
            &TelemetrySection::default(),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect("the telemetry builds"),
    );
    let log = RequestLog::default().with_metrics(&telemetry.meter("ferrohealth_telemetry"));
    let routes = || {
        Router::new()
            .route("/slow", get(std::future::pending::<&'static str>))
            .route("/fast", get(|| async { "ok" }))
    };
    let by_layer = routes().layer(log.clone());
    let by_middleware = routes().layer(axum::middleware::from_fn_with_state(log, middleware));
    let active = |telemetry: &Telemetry| {
        let text = telemetry.render_prometheus().expect("the metrics render");
        text.lines()
            .find(|line| {
                line.starts_with(
                    "http_server_active_requests{http_request_method=\"GET\",url_scheme=\"http\"",
                )
            })
            .map(|line| line.rsplit(' ').next().unwrap_or_default().to_owned())
    };
    for app in [by_layer, by_middleware] {
        {
            let mut slow = std::pin::pin!(app.clone().oneshot(get_request("/slow")));
            let waited =
                tokio::time::timeout(std::time::Duration::from_millis(20), &mut slow).await;
            assert!(waited.is_err(), "the slow request is still in flight");
            assert_eq!(Some("1".to_owned()), active(&telemetry), "counted in");
        }
        assert_eq!(
            Some("0".to_owned()),
            active(&telemetry),
            "a dropped request is counted out"
        );
        app.oneshot(get_request("/fast"))
            .await
            .expect("the request is served");
        assert_eq!(
            Some("0".to_owned()),
            active(&telemetry),
            "a finished request is counted out"
        );
    }
}
