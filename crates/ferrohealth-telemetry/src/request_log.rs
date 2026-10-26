// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One log line and one span per request.
//!
//! The line names the method, the matched route template, the status, the
//! latency, the server's request id and whether the client named one of its
//! own. A body, the raw path, a header value, a query value outside the
//! configured allow-list and the client's request id never reach it: a path
//! segment, a query value or the client's free-text id can name a patient.
//! The span carries the method, the route template and the status under
//! the OpenTelemetry HTTP semantic conventions, and is the root of a trace
//! of the server's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{Method, StatusCode};
use tracing::Instrument as _;
use tracing::field::Empty;

/// The route a request that matched none is logged under.
pub const UNMATCHED: &str = "<unmatched>";

/// The longest query value the line carries, in characters.
pub const MAX_VALUE_LENGTH: usize = 64;

/// The `tracing` target of the request line and span.
pub const TARGET: &str = "ferrohealth_telemetry::request";

/// The server's id of one request, which the server stack puts in the
/// request's extensions before the request log runs.
///
/// The id is the server's own. Whether the client named one of its own is
/// kept as a flag: the client's text is echoed to the client and never
/// logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId {
    id: String,
    client_named: bool,
}

impl RequestId {
    /// The server's id `id`, and whether the client sent one of its own.
    #[must_use]
    pub fn new(id: impl Into<String>, client_named: bool) -> Self {
        Self {
            id: id.into(),
            client_named,
        }
    }

    /// Returns the server's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether the client sent an id of its own.
    #[must_use]
    pub const fn client_named(&self) -> bool {
        self.client_named
    }
}

/// One finished request, as a [`Recorder`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct Completed<'a> {
    /// The method.
    pub method: &'a Method,
    /// The matched route template, when a route matched.
    pub route: Option<&'a str>,
    /// The status of the response.
    pub status: StatusCode,
    /// How long the request took.
    pub latency: Duration,
}

/// Something that counts finished requests beside the log, such as the
/// usage report's latency window.
pub trait Recorder: Send + Sync + 'static {
    /// Records one finished request.
    fn record(&self, completed: &Completed<'_>);
}

/// The request log: the query parameters whose values it may carry, and the
/// recorders it feeds.
#[derive(Clone, Default)]
pub struct RequestLog {
    logged_query_parameters: Arc<[String]>,
    recorders: Vec<Arc<dyn Recorder>>,
}

impl std::fmt::Debug for RequestLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestLog")
            .field("logged_query_parameters", &self.logged_query_parameters)
            .field("recorders", &self.recorders.len())
            .finish()
    }
}

impl RequestLog {
    /// A log that carries the values of `logged_query_parameters` and no
    /// other query value.
    #[must_use]
    pub fn new(logged_query_parameters: &[String]) -> Self {
        Self {
            logged_query_parameters: logged_query_parameters.into(),
            recorders: Vec::new(),
        }
    }

    /// Feeds every finished request to `recorder` too.
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn Recorder>) -> Self {
        self.recorders.push(recorder);
        self
    }
}

/// The middleware: mount it with
/// `axum::middleware::from_fn_with_state(log, request_log::middleware)` as a
/// route layer, so the matched route is known.
pub async fn middleware(State(log): State<RequestLog>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let route = matched.as_deref().unwrap_or(UNMATCHED).to_owned();
    let query = allowed_parameters(
        request.uri().query().unwrap_or_default(),
        &log.logged_query_parameters,
    );
    let (request_id, client_named) = request
        .extensions()
        .get::<RequestId>()
        .map_or((String::new(), false), |id| {
            (id.id().to_owned(), id.client_named())
        });
    let span = tracing::info_span!(
        target: TARGET,
        "request",
        http.request.method = method.as_str(),
        http.route = route.as_str(),
        http.response.status_code = Empty,
        request_id = request_id.as_str(),
    );
    let started = Instant::now();
    let response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    let latency = started.elapsed();
    span.record("http.response.status_code", status.as_u16());
    let completed = Completed {
        method: &method,
        route: matched.as_deref(),
        status,
        latency,
    };
    for recorder in &log.recorders {
        recorder.record(&completed);
    }
    let latency_ms = latency.as_secs_f64() * 1000.0;
    let (method, route, query, request_id) = (
        method.as_str(),
        route.as_str(),
        query.as_str(),
        request_id.as_str(),
    );
    let status = status.as_u16();
    if completed.status.is_server_error() {
        tracing::error!(target: TARGET, method, route, status, latency_ms, query, request_id, client_named, "request");
    } else if completed.status.is_client_error() {
        tracing::warn!(target: TARGET, method, route, status, latency_ms, query, request_id, client_named, "request");
    } else {
        tracing::info!(target: TARGET, method, route, status, latency_ms, query, request_id, client_named, "request");
    }
    response
}

/// Returns the allowed `key=value` pairs of `query`, each value cut to
/// [`MAX_VALUE_LENGTH`] characters, joined by spaces.
fn allowed_parameters(query: &str, allowed: &[String]) -> String {
    let mut out = String::new();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if !allowed.iter().any(|name| name == key) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(key);
        out.push('=');
        out.extend(value.chars().take(MAX_VALUE_LENGTH));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use opentelemetry::trace::{SpanId, TraceId};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tower::ServiceExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{
        Completed, MAX_VALUE_LENGTH, Recorder, RequestId, RequestLog, allowed_parameters,
        middleware,
    };
    use crate::section::{Format, TelemetrySection};
    use crate::telemetry::{Service, Telemetry};

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

    #[tokio::test]
    async fn the_line_names_the_template_and_the_server_id_and_nothing_a_client_wrote() {
        let capture = Capture::default();
        let section = TelemetrySection {
            format: Format::Json,
            ..TelemetrySection::default()
        };
        let telemetry = Telemetry::new(&section, Service::new("ferrosys", "0.0.0"))
            .expect("the telemetry builds");
        let subscriber = tracing_subscriber::registry().with(
            telemetry
                .layers(&section, false, capture.clone())
                .expect("the layers build"),
        );
        let count = Arc::new(Count::default());
        let log = RequestLog::new(&["_count".to_owned()]).with_recorder(count.clone());
        let _default = tracing::subscriber::set_default(subscriber);
        let response = app(log)
            .oneshot(request())
            .await
            .expect("the request is served");
        assert_eq!(http::StatusCode::OK, response.status());
        let text = capture.text();
        let line: serde_json::Value = serde_json::from_str(text.trim_end()).expect("one JSON line");
        assert_eq!("/fhir/Patient/{id}", line["route"]);
        assert_eq!("GET", line["method"]);
        assert_eq!(200, line["status"]);
        assert_eq!("_count=10", line["query"]);
        assert_eq!("srv-1", line["request_id"]);
        assert_eq!(true, line["client_named"]);
        for leaked in ["999911120", "Qz7query", "Qz7client", "Qz7body", "4bf92f35"] {
            assert!(!text.contains(leaked), "the line carries {leaked}: {text}");
        }
        assert_eq!(
            vec![("/fhir/Patient/{id}".to_owned(), 200)],
            *count.0.lock().expect("the list")
        );
    }

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
                .layers(&section, false, std::io::sink)
                .expect("the layers build"),
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
        assert_eq!("request", span.name);
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
    fn nothing_is_logged_when_the_allowlist_is_empty() {
        assert_eq!("", allowed_parameters("subject_id=1234&_count=10", &[]));
    }

    #[test]
    fn only_an_allowed_parameter_contributes_and_its_value_is_cut() {
        let allowed = ["_count".to_owned()];
        assert_eq!(
            "_count=10",
            allowed_parameters("subject_id=1234&_count=10", &allowed)
        );
        let long = "x".repeat(MAX_VALUE_LENGTH + 10);
        assert_eq!(
            format!("_count={}", "x".repeat(MAX_VALUE_LENGTH)),
            allowed_parameters(&format!("_count={long}"), &allowed)
        );
        assert_eq!("", allowed_parameters("_count", &allowed));
    }
}
