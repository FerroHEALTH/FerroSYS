// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The HTTP server metrics: the duration histogram and the active-request
//! counter, read through the Prometheus route.

use std::sync::Arc;

use crate::request_log::{RequestLog, middleware};
use crate::section::TelemetrySection;
use crate::telemetry::{Service, Telemetry};
use axum::Router;
use axum::routing::get;
use tower::ServiceExt as _;

use super::get_request;

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
