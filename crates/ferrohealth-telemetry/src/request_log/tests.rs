// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the request log, one child per subject, and the capture,
//! the counter and the app every one of them shares.

mod line;
mod spans;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::routing::get;
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::request_log::{Completed, Recorder, RequestId, RequestLog, middleware};
use crate::section::{Format, TelemetrySection};
use crate::telemetry::{Console, Service, Telemetry};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("the buffer").clone()).expect("UTF-8")
    }
}

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

#[derive(Default)]
struct Count(Mutex<Vec<(String, u16)>>);

impl Recorder for Count {
    fn record(&self, completed: &Completed<'_>) {
        self.0.lock().expect("the list").push((
            completed.route.unwrap_or("-").to_owned(),
            completed.status.as_u16(),
        ));
    }
}

fn app(log: RequestLog) -> Router {
    Router::new()
        .route(
            "/fhir/Patient/{id}",
            get(|| async { "{\"resourceType\":\"Patient\",\"id\":\"Qz7body\"}" }),
        )
        .route_layer(axum::middleware::from_fn_with_state(log, middleware))
        .layer(axum::middleware::map_request(
            |mut request: axum::extract::Request| async move {
                request
                    .extensions_mut()
                    .insert(RequestId::new("srv-1", true));
                request
            },
        ))
}

fn request() -> http::Request<Body> {
    http::Request::builder()
        .uri("/fhir/Patient/999911120?subject=Qz7query&_count=10")
        .header("x-request-id", "Qz7client")
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .body(Body::empty())
        .expect("the request builds")
}

fn json_lines(
    log: RequestLog,
    app: impl FnOnce(RequestLog) -> Router,
    request: http::Request<Body>,
) -> (http::StatusCode, Vec<serde_json::Value>) {
    let capture = Capture::default();
    let section = TelemetrySection {
        format: Format::Json,
        ..TelemetrySection::default()
    };
    let telemetry =
        Telemetry::new(&section, Service::new("ferrosys", "0.0.0")).expect("the telemetry builds");
    let subscriber = tracing_subscriber::registry().with(
        telemetry
            .layers(&section, &Console::new(false, None), capture.clone())
            .expect("the layers build")
            .layers,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("the runtime builds");
    let status = tracing::subscriber::with_default(subscriber, || {
        runtime
            .block_on(app(log).oneshot(request))
            .expect("the request is served")
            .status()
    });
    let lines = capture
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    (status, lines)
}

fn get_request(uri: &str) -> http::Request<Body> {
    http::Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("the request builds")
}
