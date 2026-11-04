// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One log line and one span per request.
//!
//! The line names the method, the matched route template, the status, the
//! latency, the server's request id and whether the client named one of its
//! own. A body, the raw path, a header value, a query value outside the
//! configured allow-list or of the wrong [`ValueShape`] and the client's
//! request id never reach it: a path
//! segment, a query value or the client's free-text id can name a patient.
//! The span carries the method, the route template and the status under
//! the OpenTelemetry semantic conventions v1.44.0
//! (`docs/http/http-spans.md`, HTTP server span), and is the root of a
//! trace of the server's own. It is a server span named `{method} {route}`,
//! and its status is `Error` for a 5xx. A method outside the known set is
//! `_OTHER` in the line, the span and the histogram ([`method_label`]), and
//! the client's token is kept out of all three: `http.request.method_original`
//! would carry text the client wrote.
//!
//! With [`RequestLog::with_metrics`] every request is also recorded in the
//! `http.server.request.duration` histogram and counted in
//! `http.server.active_requests` while in flight (semantic conventions
//! v1.44.0, `docs/http/http-metrics.md`). A handler adds fields of its own to the line with
//! [`fields::LineFields`] in its response's extensions, and a [`RouteNamer`] names
//! the route of a request no route matched.

pub mod fields;
mod query;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{Method, StatusCode, Version};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter, UpDownCounter};
use tracing::field::Empty;
use tracing::instrument::Instrumented;
use tracing::{Instrument as _, Level};

use self::fields::LineFields;
use self::query::allowed_parameters;

/// The route a request that matched none is logged under.
pub const UNMATCHED: &str = "<unmatched>";

/// The longest query value the line carries, in characters.
pub const MAX_VALUE_LENGTH: usize = 64;

/// The shape a logged query value must have to reach the line.
///
/// A value of another shape is left out of the line with its key, so an
/// allow-listed name cannot carry text of the wrong kind, such as an
/// identifier sent as `offset`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueShape {
    /// Any text, cut to [`MAX_VALUE_LENGTH`] characters.
    #[default]
    Text,
    /// One to `max` ASCII digits, and nothing else: a sign, a space or a
    /// percent-encoded digit leaves the value out.
    Digits {
        /// The most digits the value may have.
        max: usize,
    },
}

impl ValueShape {
    /// Returns `value` as the line carries it, or `None` when it does not
    /// have this shape.
    fn admit(self, value: &str) -> Option<&str> {
        match self {
            Self::Text => Some(value),
            Self::Digits { max } => (!value.is_empty()
                && value.len() <= max
                && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then_some(value),
        }
    }
}

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
    id: Arc<str>,
    client_named: bool,
}

impl RequestId {
    /// The server's id `id`, and whether the client sent one of its own.
    #[must_use]
    pub fn new(id: impl Into<String>, client_named: bool) -> Self {
        Self {
            id: Arc::from(id.into()),
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

/// The name of the up-down counter of the requests in flight.
///
/// OpenTelemetry semantic conventions v1.44.0, `docs/http/http-metrics.md`,
/// metric `http.server.active_requests`; Prometheus reads it as
/// `http_server_active_requests`.
pub const ACTIVE_REQUESTS: &str = "http.server.active_requests";

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
///
/// It is also a [`tower::Layer`], which records each request with no boxed
/// future, where [`middleware`] boxes one through `axum::middleware`.
#[derive(Clone, Default)]
pub struct RequestLog {
    settings: Arc<Settings>,
}

/// What a [`RequestLog`] holds, shared by its clones: a stack clones its
/// layers for every request.
#[derive(Clone, Default)]
struct Settings {
    logged_query_parameters: Arc<[String]>,
    shapes: BTreeMap<String, ValueShape>,
    default_shape: ValueShape,
    recorders: Vec<Arc<dyn Recorder>>,
    duration: Option<Histogram<f64>>,
    active: Option<UpDownCounter<i64>>,
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
        let settings = &self.settings;
        f.debug_struct("RequestLog")
            .field("logged_query_parameters", &settings.logged_query_parameters)
            .field("shapes", &settings.shapes)
            .field("default_shape", &settings.default_shape)
            .field("recorders", &settings.recorders.len())
            .field("metrics", &settings.duration.is_some())
            .field("scheme", &settings.scheme)
            .field("namer", &settings.namer.is_some())
            .finish()
    }
}

impl RequestLog {
    /// A log that carries the values of `logged_query_parameters` and no
    /// other query value.
    #[must_use]
    pub fn new(logged_query_parameters: &[String]) -> Self {
        Self {
            settings: Arc::new(Settings {
                logged_query_parameters: logged_query_parameters.into(),
                ..Settings::default()
            }),
        }
    }

    /// Requires the logged value of the query parameter `name` to have
    /// `shape`, over the shape of [`RequestLog::with_default_query_shape`].
    ///
    /// The shape admits nothing on its own: only a name the log was created
    /// with reaches the line.
    ///
    /// # Examples
    ///
    /// ```
    /// use ferrohealth_telemetry::request_log::{RequestLog, ValueShape};
    ///
    /// // `offset=20` is logged; `offset=SYNTHETIC-1` is left out with its key.
    /// let log = RequestLog::new(&["offset".to_owned(), "fetch".to_owned()])
    ///     .with_query_shape("offset", ValueShape::Digits { max: 10 })
    ///     .with_query_shape("fetch", ValueShape::Digits { max: 10 });
    /// # let _ = log;
    /// ```
    #[must_use]
    pub fn with_query_shape(mut self, name: impl Into<String>, shape: ValueShape) -> Self {
        Arc::make_mut(&mut self.settings)
            .shapes
            .insert(name.into(), shape);
        self
    }

    /// Requires every logged query value without a shape of its own to have
    /// `shape`; without this call it is [`ValueShape::Text`].
    #[must_use]
    pub fn with_default_query_shape(mut self, shape: ValueShape) -> Self {
        Arc::make_mut(&mut self.settings).default_shape = shape;
        self
    }

    /// Feeds every finished request to `recorder` too.
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn Recorder>) -> Self {
        Arc::make_mut(&mut self.settings).recorders.push(recorder);
        self
    }

    /// Records every finished request in the [`REQUEST_DURATION`] histogram
    /// of `meter`, such as `Telemetry::meter`'s, so Prometheus and the OTLP
    /// push read one series, and every request in flight in the
    /// [`ACTIVE_REQUESTS`] up-down counter.
    ///
    /// A request is counted in when it begins and out when its future
    /// completes or is dropped, so a request whose client goes away is
    /// counted out too.
    #[must_use]
    pub fn with_metrics(mut self, meter: &Meter) -> Self {
        let settings = Arc::make_mut(&mut self.settings);
        settings.duration = Some(
            meter
                .f64_histogram(REQUEST_DURATION)
                .with_description("Duration of HTTP server requests.")
                .with_unit("s")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
        );
        settings.active = Some(
            meter
                .i64_up_down_counter(ACTIVE_REQUESTS)
                .with_description("Number of active HTTP server requests.")
                .with_unit("{request}")
                .build(),
        );
        self
    }

    /// Records `url.scheme` as `https` when the listener serves TLS itself,
    /// and as `http`, the default, when it does not.
    #[must_use]
    pub fn serving_tls(mut self, tls: bool) -> Self {
        Arc::make_mut(&mut self.settings).scheme = if tls { Scheme::Https } else { Scheme::Http };
        self
    }

    /// Names the route of a request no route matched through `namer`.
    #[must_use]
    pub fn with_route_namer(mut self, namer: Arc<dyn RouteNamer>) -> Self {
        Arc::make_mut(&mut self.settings).namer = Some(namer);
        self
    }

    /// The `url.scheme` of the requests this log sees.
    fn scheme(&self) -> &'static str {
        match self.settings.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    /// Counts a request of `method` in, until the returned guard drops.
    fn in_flight(&self, method: &Method) -> Option<InFlight> {
        let counter = self.settings.active.clone()?;
        let attributes = [
            KeyValue::new("http.request.method", method_label(method)),
            KeyValue::new("url.scheme", self.scheme()),
        ];
        counter.add(1, &attributes);
        Some(InFlight {
            counter,
            attributes,
        })
    }

    /// Records one finished request in the duration histogram.
    fn measure(&self, completed: &Completed<'_>, version: Version) {
        let Some(duration) = &self.settings.duration else {
            return;
        };
        let mut attributes = vec![
            KeyValue::new("http.request.method", method_label(completed.method)),
            KeyValue::new("url.scheme", self.scheme()),
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

/// A request counted in [`ACTIVE_REQUESTS`], counted out when it drops.
struct InFlight {
    counter: UpDownCounter<i64>,
    attributes: [KeyValue; 2],
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.counter.add(-1, &self.attributes);
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

/// The name of a request's span: `{method} {route}`, with `HTTP` for the
/// method when it is [`OTHER_METHOD`] and the method alone when no route
/// matched or was named.
///
/// Semantic conventions v1.44.0, `docs/http/http-spans.md`, Name: the
/// target of a server span is `http.route`, never the path.
fn span_name(label: &str, route: Option<&str>) -> String {
    let method = if label == OTHER_METHOD { "HTTP" } else { label };
    match route {
        Some(route) => format!("{method} {route}"),
        None => method.to_owned(),
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
/// [`RouteNamer`] is never asked. The [`RequestLog`] layer does the same
/// without the boxing of `axum::middleware`.
pub async fn middleware(State(log): State<RequestLog>, request: Request, next: Next) -> Response {
    let begun = Begun::of(&log, &request);
    let response = Serving::new(next.run(request), begun.serving())
        .instrument(begun.span.clone())
        .await;
    begun.finish(&log, &response);
    response
}

impl<S> tower::Layer<S> for RequestLog {
    type Service = RequestLogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RequestLogService {
            log: self.clone(),
            inner,
        }
    }
}

/// The service of the [`RequestLog`] layer: one line, one span and one
/// sample per request `inner` answers.
#[derive(Clone, Debug)]
pub struct RequestLogService<S> {
    log: RequestLog,
    inner: S,
}

impl<S, B, ResB> tower::Service<http::Request<B>> for RequestLogService<S>
where
    S: tower::Service<http::Request<B>, Response = http::Response<ResB>>,
{
    type Response = http::Response<ResB>;
    type Error = S::Error;
    type Future = RequestLogFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let begun = Begun::of(&self.log, &request);
        let serving = begun.serving();
        let inner = {
            let _serving = crate::panic::serving(serving.clone());
            self.inner.call(request)
        };
        RequestLogFuture {
            inner: Serving::new(inner, serving).instrument(begun.span.clone()),
            begun: Some(begun),
            log: self.log.clone(),
        }
    }
}

pin_project_lite::pin_project! {
    /// The future of [`RequestLogService`]: writes the line when the
    /// response is ready.
    pub struct RequestLogFuture<F> {
        #[pin]
        inner: Instrumented<Serving<F>>,
        begun: Option<Begun>,
        log: RequestLog,
    }
}

pin_project_lite::pin_project! {
    /// A request's future that names the request to the panic hook while it
    /// is polled ([`crate::panic::record`]).
    struct Serving<F> {
        #[pin]
        inner: F,
        request_id: Option<Arc<str>>,
    }
}

impl<F> Serving<F> {
    /// Wraps `inner`, the future serving the request `request_id`.
    const fn new(inner: F, request_id: Option<Arc<str>>) -> Self {
        Self { inner, request_id }
    }
}

impl<F: Future> Future for Serving<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _serving = crate::panic::serving(this.request_id.clone());
        this.inner.poll(cx)
    }
}

impl<F> std::fmt::Debug for RequestLogFuture<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestLogFuture").finish_non_exhaustive()
    }
}

impl<F, ResB, E> Future for RequestLogFuture<F>
where
    F: Future<Output = Result<http::Response<ResB>, E>>,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let output = std::task::ready!(this.inner.poll(cx));
        if let (Ok(response), Some(begun)) = (&output, this.begun.take()) {
            begun.finish(this.log, response);
        }
        Poll::Ready(output)
    }
}

/// The route a request is logged under.
enum Route {
    /// The template the router matched.
    Matched(MatchedPath),
    /// The template a [`RouteNamer`] named.
    Named(String),
    /// No route.
    Unmatched,
}

impl Route {
    /// The template, when a route matched or was named.
    fn template(&self) -> Option<&str> {
        match self {
            Self::Matched(matched) => Some(matched.as_str()),
            Self::Named(named) => Some(named),
            Self::Unmatched => None,
        }
    }
}

/// What the request log reads from a request before it is answered.
struct Begun {
    method: Method,
    version: Version,
    route: Route,
    query: String,
    request_id: Option<RequestId>,
    span: tracing::Span,
    started: Instant,
    /// Counts the request out of [`ACTIVE_REQUESTS`] when this drops, the
    /// request finished or abandoned.
    _in_flight: Option<InFlight>,
}

impl Begun {
    /// Reads `request` and opens its span.
    fn of<B>(log: &RequestLog, request: &http::Request<B>) -> Self {
        let method = request.method().clone();
        let route = match request.extensions().get::<MatchedPath>() {
            Some(matched) => Route::Matched(matched.clone()),
            None => log
                .settings
                .namer
                .as_ref()
                .and_then(|namer| namer.route(&method, request.uri().path()))
                .map_or(Route::Unmatched, Route::Named),
        };
        let query = allowed_parameters(request.uri().query().unwrap_or_default(), &log.settings);
        let request_id = request.extensions().get::<RequestId>().cloned();
        // NOTE: semantic conventions v1.44.0, `docs/http/http-spans.md`, [5]: by our own
        // design `http.request.method_original` is left out, as text the client wrote.
        let label = method_label(&method);
        let span = tracing::info_span!(
            target: TARGET,
            "request",
            otel.name = span_name(label, route.template()),
            otel.kind = "server",
            otel.status_code = Empty,
            http.request.method = label,
            http.route = route.template().unwrap_or(UNMATCHED),
            http.response.status_code = Empty,
            request_id = request_id.as_ref().map_or("", RequestId::id),
        );
        let in_flight = log.in_flight(&method);
        Self {
            method,
            version: request.version(),
            route,
            query,
            request_id,
            span,
            started: Instant::now(),
            _in_flight: in_flight,
        }
    }

    /// The server's id of the request, for the panic hook.
    fn serving(&self) -> Option<Arc<str>> {
        self.request_id.as_ref().map(|id| Arc::clone(&id.id))
    }

    /// Records the answered request: the span's status, the recorders, the
    /// histogram and the line.
    fn finish<ResB>(self, log: &RequestLog, response: &http::Response<ResB>) {
        let status = response.status();
        let latency = self.started.elapsed();
        self.span
            .record("http.response.status_code", status.as_u16());
        // NOTE: semantic conventions v1.44.0, `docs/http/http-spans.md`, Status: a server
        // span's status is `Error` for a 5xx and left unset for every other code.
        if status.is_server_error() {
            self.span.record("otel.status_code", "ERROR");
        }
        let completed = Completed {
            method: &self.method,
            route: self.route.template(),
            status,
            latency,
        };
        for recorder in &log.settings.recorders {
            recorder.record(&completed);
        }
        log.measure(&completed, self.version);
        let latency_ms = latency.as_secs_f64() * 1000.0;
        let (method, route, query, request_id) = (
            method_label(&self.method),
            completed.route.unwrap_or(UNMATCHED),
            self.query.as_str(),
            self.request_id.as_ref().map_or("", RequestId::id),
        );
        let client_named = self
            .request_id
            .as_ref()
            .is_some_and(RequestId::client_named);
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
    }
}
