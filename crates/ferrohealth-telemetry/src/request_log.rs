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
//! the OpenTelemetry semantic conventions v1.44.0
//! (`docs/http/http-spans.md`, HTTP server span), and is the root of a
//! trace of the server's own.
//!
//! With [`RequestLog::with_metrics`] every request is also recorded in the
//! `http.server.request.duration` histogram (semantic conventions v1.44.0,
//! `docs/http/http-metrics.md`). A handler adds fields of its own to the line with
//! [`fields::LineFields`] in its response's extensions, and a [`RouteNamer`] names
//! the route of a request no route matched.

pub mod fields;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{Method, StatusCode, Version};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use tracing::field::Empty;
use tracing::{Instrument as _, Level};

use self::fields::LineFields;

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

/// The name of the histogram of request durations, in seconds.
///
/// OpenTelemetry semantic conventions v1.44.0, `docs/http/http-metrics.md`,
/// metric `http.server.request.duration`; Prometheus reads it as
/// `http_server_request_duration_seconds`.
pub const REQUEST_DURATION: &str = "http.server.request.duration";

/// The bucket boundaries of [`REQUEST_DURATION`], in seconds: the
/// `ExplicitBucketBoundaries` advice of the metric's definition.
pub const DURATION_BUCKETS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// The `http.request.method` of a method the semantic conventions do not
/// know (v1.44.0, `docs/registry/attributes/http.md`).
pub const OTHER_METHOD: &str = "_OTHER";

/// One finished request, as a [`Recorder`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct Completed<'a> {
    /// The method.
    pub method: &'a Method,
    /// The matched route template, or the route a [`RouteNamer`] named for
    /// a request no route matched.
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

/// Names the route of a request no route matched, such as the template of
/// the protocol operation its path addresses.
///
/// The name is logged, traced and recorded as `http.route`, so it must be a
/// template from a closed set the product owns, never the raw path.
pub trait RouteNamer: Send + Sync + 'static {
    /// Returns the route template of a request of `method` to `path`, or
    /// `None` to log it under [`UNMATCHED`].
    fn route(&self, method: &Method, path: &str) -> Option<String>;
}

/// The request log: the query parameters whose values it may carry, the
/// recorders it feeds, the duration histogram and the namer of unmatched
/// routes.
#[derive(Clone, Default)]
pub struct RequestLog {
    logged_query_parameters: Arc<[String]>,
    recorders: Vec<Arc<dyn Recorder>>,
    duration: Option<Histogram<f64>>,
    scheme: Scheme,
    namer: Option<Arc<dyn RouteNamer>>,
}

/// The `url.scheme` of the requests a listener serves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Scheme {
    /// Plain HTTP, TLS ending in front of the server when there is any.
    #[default]
    Http,
    /// TLS served by the listener itself.
    Https,
}

impl std::fmt::Debug for RequestLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestLog")
            .field("logged_query_parameters", &self.logged_query_parameters)
            .field("recorders", &self.recorders.len())
            .field("metrics", &self.duration.is_some())
            .field("scheme", &self.scheme)
            .field("namer", &self.namer.is_some())
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
            ..Self::default()
        }
    }

    /// Feeds every finished request to `recorder` too.
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn Recorder>) -> Self {
        self.recorders.push(recorder);
        self
    }

    /// Records every finished request in the [`REQUEST_DURATION`] histogram
    /// of `meter`, such as `Telemetry::meter`'s, so Prometheus and the OTLP
    /// push read one series.
    #[must_use]
    pub fn with_metrics(mut self, meter: &Meter) -> Self {
        self.duration = Some(
            meter
                .f64_histogram(REQUEST_DURATION)
                .with_description("Duration of HTTP server requests.")
                .with_unit("s")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
        );
        self
    }

    /// Records `url.scheme` as `https` when the listener serves TLS itself,
    /// and as `http`, the default, when it does not.
    #[must_use]
    pub fn serving_tls(mut self, tls: bool) -> Self {
        self.scheme = if tls { Scheme::Https } else { Scheme::Http };
        self
    }

    /// Names the route of a request no route matched through `namer`.
    #[must_use]
    pub fn with_route_namer(mut self, namer: Arc<dyn RouteNamer>) -> Self {
        self.namer = Some(namer);
        self
    }

    /// Records one finished request in the duration histogram.
    fn measure(&self, completed: &Completed<'_>, version: Version) {
        let Some(duration) = &self.duration else {
            return;
        };
        let scheme = match self.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        let mut attributes = vec![
            KeyValue::new("http.request.method", method_label(completed.method)),
            KeyValue::new("url.scheme", scheme),
            KeyValue::new(
                "http.response.status_code",
                i64::from(completed.status.as_u16()),
            ),
        ];
        if let Some(route) = completed.route {
            attributes.push(KeyValue::new("http.route", route.to_owned()));
        }
        if let Some(version) = protocol_version(version) {
            attributes.push(KeyValue::new("network.protocol.version", version));
        }
        // NOTE: semantic conventions v1.44.0, `docs/http/http-spans.md`, Status: a 5xx is a
        // server span's error, so `error.type` is its status code and else unset.
        if completed.status.is_server_error() {
            attributes.push(KeyValue::new(
                "error.type",
                completed.status.as_str().to_owned(),
            ));
        }
        duration.record(completed.latency.as_secs_f64(), &attributes);
    }
}

/// The `http.request.method` of `method`: its name for a method the
/// semantic conventions know, and [`OTHER_METHOD`] for any other.
///
/// The known methods are those of RFC 9110 §9, `PATCH` of RFC 5789 and
/// `QUERY` (semantic conventions v1.44.0,
/// `docs/registry/attributes/http.md`), so a client cannot mint a series.
#[must_use]
pub fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::CONNECT => "CONNECT",
        Method::DELETE => "DELETE",
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        Method::PATCH => "PATCH",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::TRACE => "TRACE",
        _ if method.as_str() == "QUERY" => "QUERY",
        _ => OTHER_METHOD,
    }
}

/// The `network.protocol.version` of `version`.
const fn protocol_version(version: Version) -> Option<&'static str> {
    match version {
        Version::HTTP_09 => Some("0.9"),
        Version::HTTP_10 => Some("1.0"),
        Version::HTTP_11 => Some("1.1"),
        Version::HTTP_2 => Some("2"),
        Version::HTTP_3 => Some("3"),
        _ => None,
    }
}

/// The middleware: mount it with
/// `axum::middleware::from_fn_with_state(log, request_log::middleware)`.
///
/// Mounted with `Router::layer` it sees every request, the fallback's
/// included, and the matched route of each; mounted with
/// `Router::route_layer` it sees only the requests a route matched, so a
/// [`RouteNamer`] is never asked.
pub async fn middleware(State(log): State<RequestLog>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let version = request.version();
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .or_else(|| {
            log.namer
                .as_ref()
                .and_then(|namer| namer.route(&method, request.uri().path()))
        });
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
    log.measure(&completed, version);
    let latency_ms = latency.as_secs_f64() * 1000.0;
    let (method, route, query, request_id) = (
        method.as_str(),
        route.as_str(),
        query.as_str(),
        request_id.as_str(),
    );
    let status = status.as_u16();
    if let Some(added) = response
        .extensions()
        .get::<LineFields>()
        .filter(|added| !added.is_empty())
    {
        let level = if completed.status.is_server_error() {
            Level::ERROR
        } else if completed.status.is_client_error() {
            Level::WARN
        } else {
            Level::INFO
        };
        let core = fields::Core {
            method,
            route,
            status,
            latency_ms,
            query,
            request_id,
            client_named,
        };
        fields::write(level, &core, added);
    } else if completed.status.is_server_error() {
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

    use super::fields::{FieldError, LineFields, MAX_FIELDS, Token};
    use super::{
        Completed, MAX_VALUE_LENGTH, Recorder, RequestId, RequestLog, RouteNamer, UNMATCHED,
        allowed_parameters, method_label, middleware,
    };
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
                .layers(&section, &Console::new(false, None), capture.clone())
                .expect("the layers build")
                .layers,
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
        let telemetry = Telemetry::new(&section, Service::new("ferrosys", "0.0.0"))
            .expect("the telemetry builds");
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

    #[test]
    fn the_fields_a_handler_adds_are_named_fields_of_the_line() {
        let app = |log: RequestLog| {
            Router::new()
                .route(
                    "/pix/feed",
                    get(|| async {
                        let added = LineFields::new()
                            .with("caller", Token::new("ferropix-test").expect("a token"))
                            .and_then(|added| {
                                added.with(
                                    "domain",
                                    Token::new("2.16.840.1.113883.2.4.6.3").expect("a token"),
                                )
                            })
                            .expect("the fields are allowed");
                        let mut response = axum::response::IntoResponse::into_response((
                            http::StatusCode::CONFLICT,
                            "Qz7body",
                        ));
                        response.extensions_mut().insert(added);
                        response
                    }),
                )
                .layer(axum::middleware::from_fn_with_state(log, middleware))
        };
        let (status, lines) = json_lines(
            RequestLog::default(),
            app,
            get_request("/pix/feed?id=Qz7query"),
        );
        assert_eq!(http::StatusCode::CONFLICT, status);
        let [line] = lines.as_slice() else {
            panic!("one line: {lines:?}");
        };
        assert_eq!("ferropix-test", line["caller"]);
        assert_eq!("2.16.840.1.113883.2.4.6.3", line["domain"]);
        assert_eq!("/pix/feed", line["route"]);
        assert_eq!(409, line["status"]);
        assert_eq!("request", line["message"]);
        assert_eq!("WARN", line["level"], "a client error is a warning");
        assert_eq!("ferrohealth_telemetry::request", line["target"]);
        let text = line.to_string();
        assert!(!text.contains("Qz7"), "{text}");
    }

    #[test]
    fn a_line_field_is_a_lower_case_name_and_a_short_visible_token() {
        let token = || Token::new("x").expect("a token");
        assert_eq!(
            Err(FieldError::Reserved { name: "route" }),
            LineFields::new().with("route", token())
        );
        assert_eq!(
            Err(FieldError::Name { name: "Caller" }),
            LineFields::new().with("Caller", token())
        );
        assert_eq!(
            Err(FieldError::Name { name: "" }),
            LineFields::new().with("", token())
        );
        for value in ["", "two words", "line\nbreak", "é", &"x".repeat(129)] {
            assert_eq!(Err(FieldError::Value), Token::new(value), "{value:?}");
        }
        assert!(Token::new("http://snomed.info/sct").is_ok());
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let mut full = LineFields::new();
        for name in names.iter().take(MAX_FIELDS) {
            full.insert(name, token()).expect("room for the field");
        }
        assert_eq!(Err(FieldError::Full), full.insert("i", token()));
        full.insert("a", Token::new("again").expect("a token"))
            .expect("a known name is replaced");
        assert_eq!(Some(("a", "again")), full.iter().next());
        assert_eq!(MAX_FIELDS, full.len());
    }

    struct Operations;

    impl RouteNamer for Operations {
        fn route(&self, method: &http::Method, path: &str) -> Option<String> {
            (method == http::Method::GET && path.starts_with("/v1/ehr/"))
                .then(|| "/v1/ehr/{ehr_id}".to_owned())
        }
    }

    #[test]
    fn a_route_namer_names_a_request_no_route_matched() {
        let app = |log: RequestLog| {
            Router::new()
                .route("/known", get(|| async { "ok" }))
                .layer(axum::middleware::from_fn_with_state(log, middleware))
        };
        let count = Arc::new(Count::default());
        let named = RequestLog::default()
            .with_route_namer(Arc::new(Operations))
            .with_recorder(count.clone());
        let (status, lines) = json_lines(named.clone(), app, get_request("/v1/ehr/999911120"));
        assert_eq!(http::StatusCode::NOT_FOUND, status);
        assert_eq!(
            Some("/v1/ehr/{ehr_id}"),
            lines.first().and_then(|line| line["route"].as_str())
        );
        assert!(!format!("{lines:?}").contains("999911120"));
        let (_, lines) = json_lines(named, app, get_request("/elsewhere"));
        assert_eq!(
            Some(UNMATCHED),
            lines.first().and_then(|line| line["route"].as_str())
        );
        let (_, lines) = json_lines(RequestLog::default(), app, get_request("/v1/ehr/1"));
        assert_eq!(
            Some(UNMATCHED),
            lines.first().and_then(|line| line["route"].as_str())
        );
        assert_eq!(
            vec![("/v1/ehr/{ehr_id}".to_owned(), 404), ("-".to_owned(), 404)],
            *count.0.lock().expect("the list"),
            "the recorders see the named route too"
        );
    }

    #[test]
    fn a_method_outside_the_known_set_is_other() {
        assert_eq!("GET", method_label(&http::Method::GET));
        assert_eq!(
            "QUERY",
            method_label(&http::Method::from_bytes(b"QUERY").expect("a method"))
        );
        assert_eq!(
            "_OTHER",
            method_label(&http::Method::from_bytes(b"SYNTHETIC").expect("a method"))
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
