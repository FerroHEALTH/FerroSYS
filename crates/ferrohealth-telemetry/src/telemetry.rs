// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one OpenTelemetry provider of a process, and the subscriber that
//! carries the console and the span export.
//!
//! One meter provider feeds every reader, so an instrument cannot exist on
//! the Prometheus surface and not in the OTLP push. One tracer provider
//! exports the product's own spans, never a `tracing` event and never a span
//! of another crate. No propagator is installed and no request header is
//! read, so a client's `traceparent` never becomes the parent of a server
//! span (W3C Trace Context): every request starts a trace of the server's
//! own.

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, IsTerminal as _};
use std::sync::Arc;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider as _};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};
use tracing::{Metadata, Subscriber};
use tracing_subscriber::filter::{EnvFilter, FilterExt as _, FilterFn, LevelFilter};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer, reload};

use crate::console::{self, BoxedLayer};
use crate::section::{Filter, FilterError, Format, SampleRatio, TelemetrySection};

/// The service the telemetry describes: the OpenTelemetry resource of every
/// span and metric, and the crates whose spans are exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    name: String,
    version: String,
    instance_id: Option<String>,
    environment: Option<String>,
    crates: Vec<String>,
}

impl Service {
    /// Describes the service `name` (`service.name`) at `version`
    /// (`service.version`), exporting the spans of the crates whose name
    /// starts with `name`, such as `ferropix` and `ferropix_server`.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            crates: vec![name.clone()],
            name,
            version: version.into(),
            instance_id: None,
            environment: None,
        }
    }

    /// Adds the installation's id as `service.instance.id`.
    #[must_use]
    pub fn instance_id(mut self, id: impl Into<String>) -> Self {
        self.instance_id = Some(id.into());
        self
    }

    /// Names the deployment environment as `deployment.environment.name`
    /// (OpenTelemetry semantic conventions v1.44.0,
    /// `docs/resource/deployment-environment.md`), such as
    /// `production`.
    #[must_use]
    pub fn environment(mut self, name: impl Into<String>) -> Self {
        self.environment = Some(name.into());
        self
    }

    /// Exports the spans of the crates whose name starts with `prefix` too.
    #[must_use]
    pub fn export_crate(mut self, prefix: impl Into<String>) -> Self {
        self.crates.push(prefix.into());
        self
    }

    /// Returns the service name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the OpenTelemetry resource of the service.
    #[must_use]
    pub fn resource(&self) -> Resource {
        let mut attributes = vec![KeyValue::new("service.version", self.version.clone())];
        if let Some(id) = &self.instance_id {
            attributes.push(KeyValue::new("service.instance.id", id.clone()));
        }
        if let Some(environment) = &self.environment {
            attributes.push(KeyValue::new(
                "deployment.environment.name",
                environment.clone(),
            ));
        }
        Resource::builder()
            .with_service_name(self.name.clone())
            .with_attributes(attributes)
            .build()
    }

    /// Whether the span export sees `metadata`: a span, never an event, of
    /// one of the service's crates or of this crate's request span.
    #[must_use]
    pub fn exports(&self, metadata: &Metadata<'_>) -> bool {
        self.exports_target(metadata.target(), metadata.is_span())
    }

    /// Whether a span (`is_span`) or an event of `target` is exported.
    fn exports_target(&self, target: &str, is_span: bool) -> bool {
        // NOTE: no specification governs this: our own design; an event may carry a
        // value the product stripped from its spans, so only spans leave the process.
        let krate = target.split("::").next().unwrap_or_default();
        is_span
            && (krate == env!("CARGO_CRATE_NAME")
                || self
                    .crates
                    .iter()
                    .any(|prefix| krate.starts_with(prefix.as_str())))
    }
}

/// The telemetry could not be built, installed or stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InitError {
    /// The log filter does not parse.
    #[error(transparent)]
    Filter(#[from] FilterError),
    /// The console filter could not be replaced: the subscriber is gone.
    #[error("the log filter could not be replaced")]
    Reload {
        /// What `tracing-subscriber` reported.
        #[source]
        source: reload::Error,
    },
    /// A subscriber is already installed in this process.
    #[error("a log subscriber is already installed")]
    AlreadyInstalled {
        /// What `tracing-subscriber` reported.
        #[source]
        source: tracing_subscriber::util::TryInitError,
    },
    /// `[telemetry.otlp]` is present and names no endpoint.
    #[error("telemetry.otlp.endpoint is not set, and its table is present")]
    MissingEndpoint,
    /// `[telemetry.otlp]` is present and this build has no OTLP export.
    #[error("telemetry.otlp is set, but this build has no OTLP export")]
    NoOtlp,
    /// A TLS file of `[telemetry.otlp]` could not be read.
    #[error("{key} names {}, which could not be read", path.display())]
    TlsFile {
        /// The key that names the file.
        key: &'static str,
        /// The file.
        path: std::path::PathBuf,
        /// What the file system reported.
        #[source]
        source: io::Error,
    },
    /// A TLS file of `[telemetry.otlp]` is set beside an endpoint that is
    /// not `https://`, which would export in plain text and ignore it.
    #[error(
        "{key} is set, and telemetry.otlp.endpoint is not https://, so the file would be ignored"
    )]
    TlsWithoutHttps {
        /// The key that names the file.
        key: &'static str,
    },
    /// A TLS file of `[telemetry.otlp]` holds no PEM of its kind: no
    /// certificate among the trust roots, or a client identity without a
    /// certificate or a private key. No part of the file is quoted.
    #[error("{key} names {}, which holds no {wanted}", path.display())]
    TlsPem {
        /// The key that names the file.
        key: &'static str,
        /// The file.
        path: std::path::PathBuf,
        /// What the file lacks, such as `PEM certificate`.
        wanted: &'static str,
    },
    /// The client identity of `[telemetry.otlp]` could not be read.
    #[error(transparent)]
    Identity(#[from] ferrohealth_secret::file::SecretError),
    /// An OTLP exporter could not be built.
    #[cfg(feature = "otlp")]
    #[error("the OTLP exporter could not be built")]
    Exporter {
        /// What `opentelemetry-otlp` reported.
        #[source]
        source: opentelemetry_otlp::ExporterBuildError,
    },
    /// The Prometheus reader could not be built.
    #[error("the Prometheus metrics reader could not be built")]
    Prometheus {
        /// What `opentelemetry_sdk` reported.
        #[source]
        source: opentelemetry_sdk::error::OTelSdkError,
    },
    /// The metrics could not be encoded as Prometheus text.
    #[cfg(feature = "prometheus")]
    #[error("the metrics could not be encoded")]
    Encode {
        /// What `prometheus` reported.
        #[source]
        source: prometheus::Error,
    },
    /// The encoded metrics are not UTF-8.
    #[error("the encoded metrics are not UTF-8")]
    Utf8 {
        /// What the decoder reported.
        #[source]
        source: std::string::FromUtf8Error,
    },
    /// A provider could not flush or stop its exporters.
    #[error("the {signal} provider could not shut down")]
    Shutdown {
        /// `tracer` or `meter`.
        signal: &'static str,
        /// What `opentelemetry_sdk` reported.
        #[source]
        source: opentelemetry_sdk::error::OTelSdkError,
    },
}

/// The key of the OTLP trust roots file.
const TRUST_ROOTS_KEY: &str = "telemetry.otlp.trust_roots_file";

/// The key of the OTLP client identity file.
const CLIENT_IDENTITY_KEY: &str = "telemetry.otlp.client_identity_file";

/// The one OpenTelemetry provider of a process.
///
/// Built from a [`TelemetrySection`] and a [`Service`]. With the
/// `prometheus` feature its meter provider has a Prometheus reader; with
/// `[telemetry.otlp]` and the `otlp` feature it also pushes metrics and
/// exports spans. [`Telemetry::shutdown`] flushes both before the process
/// exits.
pub struct Telemetry {
    service: Service,
    meters: SdkMeterProvider,
    tracers: Option<SdkTracerProvider>,
    #[cfg(feature = "prometheus")]
    registry: prometheus::Registry,
}

impl fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Telemetry")
            .field("service", &self.service)
            .field("traces", &self.tracers.is_some())
            .finish_non_exhaustive()
    }
}

impl Telemetry {
    /// Builds the providers `section` asks for, described by `service`.
    ///
    /// The OTLP exporters speak gRPC through `tonic`, so with
    /// `[telemetry.otlp]` this runs inside the Tokio runtime that carries the
    /// export.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::MissingEndpoint`], [`InitError::NoOtlp`],
    /// [`InitError::TlsWithoutHttps`], [`InitError::TlsFile`],
    /// [`InitError::TlsPem`] and [`InitError::Identity`] for an OTLP table
    /// this build cannot honour, `InitError::Exporter` when an exporter
    /// cannot be built, and [`InitError::Prometheus`] when the Prometheus
    /// reader cannot be built.
    pub fn new(section: &TelemetrySection, service: Service) -> Result<Self, InitError> {
        let service = match &section.environment {
            Some(environment) => service.environment(environment.clone()),
            None => service,
        };
        let resource = service.resource();
        let mut meters = SdkMeterProvider::builder().with_resource(resource.clone());
        #[cfg(feature = "prometheus")]
        let registry = prometheus::Registry::new();
        #[cfg(feature = "prometheus")]
        {
            // NOTE: no specification governs this: our own design; the scope label is
            // left off, so every series carries only the labels its instrument names.
            let pull = opentelemetry_prometheus::exporter()
                .with_registry(registry.clone())
                .scope_info_enabled(false)
                .build()
                .map_err(|source| InitError::Prometheus { source })?;
            meters = meters.with_reader(pull);
        }
        let mut tracers = None;
        if let Some(section) = &section.otlp {
            if section.endpoint.is_empty() {
                return Err(InitError::MissingEndpoint);
            }
            // NOTE: no specification governs this: our own design; TLS material beside a
            // plain-HTTP collector would be ignored, so the table is refused instead.
            if !section.endpoint.expose().starts_with("https://") {
                let files = [
                    (TRUST_ROOTS_KEY, section.trust_roots_file.is_some()),
                    (CLIENT_IDENTITY_KEY, section.client_identity_file.is_some()),
                ];
                if let Some((key, _)) = files.into_iter().find(|(_, set)| *set) {
                    return Err(InitError::TlsWithoutHttps { key });
                }
            }
            let (with_push, exported) = otlp::export(section, meters, resource)?;
            meters = with_push;
            tracers = exported;
        }
        Ok(Self {
            service,
            meters: meters.build(),
            tracers,
            #[cfg(feature = "prometheus")]
            registry,
        })
    }

    /// Returns the telemetry of `service` over `tracers`, with no reader: a
    /// test hands its own tracer provider in.
    #[cfg(all(test, feature = "axum"))]
    pub(crate) fn with_tracers(service: Service, tracers: SdkTracerProvider) -> Self {
        Self {
            service,
            meters: SdkMeterProvider::builder().build(),
            tracers: Some(tracers),
            #[cfg(feature = "prometheus")]
            registry: prometheus::Registry::new(),
        }
    }

    /// Returns the meter of `scope`, on the one provider every reader reads.
    #[must_use]
    pub fn meter(&self, scope: &'static str) -> Meter {
        self.meters.meter(scope)
    }

    /// Returns the tracer of the span export, when spans are exported.
    #[must_use]
    pub fn tracer(&self) -> Option<SdkTracer> {
        self.tracers
            .as_ref()
            .map(|provider| provider.tracer(self.service.name.clone()))
    }

    /// Returns the subscriber layers and the handle of the console filter.
    ///
    /// The console renders `section`'s format as `console` decides it,
    /// writing through `writer`, and writes what `section`'s filter or
    /// `console`'s forced directives let through. The filter decides what
    /// the console writes and nothing else, so a quieter log never thins a
    /// trace.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Filter`] when the filter does not parse, which a
    /// section read through `ferrohealth-config` never holds.
    pub fn layers<S, W>(
        &self,
        section: &TelemetrySection,
        console: &Console,
        writer: W,
    ) -> Result<Layers<S>, InitError>
    where
        S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
        W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    {
        let rendering = section.format.resolve(console.terminal);
        let ansi = console.colour(section.format);
        let (configured, handle) = reload::Layer::new(section.filter.env_filter()?);
        // NOTE: no specification governs this: our own design; a forced directive is
        // its own filter beside the configured one, so no directive there can quiet it.
        let forced = match &console.forced {
            Some(forced) => forced.env_filter()?,
            None => EnvFilter::new(LevelFilter::OFF.to_string()),
        };
        let mut layers = vec![
            console::layer(rendering, ansi, writer)
                .with_filter(configured.or(forced))
                .boxed(),
        ];
        if let Some(tracer) = self.tracer() {
            let service = self.service.clone();
            layers.push(
                tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_threads(false)
                    .with_location(false)
                    .with_filter(FilterFn::new(move |metadata| service.exports(metadata)))
                    .boxed(),
            );
        }
        Ok(Layers {
            layers,
            filter: FilterHandle::new(handle),
        })
    }

    /// Installs the process-wide subscriber on stdout and returns the handle
    /// that replaces the console filter.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Filter`] when the filter does not parse and
    /// [`InitError::AlreadyInstalled`] when the process has a subscriber.
    pub fn install(
        &self,
        section: &TelemetrySection,
        console: &Console,
    ) -> Result<FilterHandle, InitError> {
        let Layers { layers, filter } = self.layers(section, console, io::stdout)?;
        tracing_subscriber::registry()
            .with(layers)
            .try_init()
            .map_err(|source| InitError::AlreadyInstalled { source })?;
        Ok(filter)
    }

    /// Renders every metric in the Prometheus text exposition format.
    ///
    /// # Errors
    ///
    /// Returns `InitError::Encode` and [`InitError::Utf8`] when the metrics
    /// cannot be encoded.
    #[cfg(feature = "prometheus")]
    pub fn render_prometheus(&self) -> Result<String, InitError> {
        use prometheus::Encoder as _;

        let mut text = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&self.registry.gather(), &mut text)
            .map_err(|source| InitError::Encode { source })?;
        String::from_utf8(text).map_err(|source| InitError::Utf8 { source })
    }

    /// Flushes and stops the tracer and the meter provider.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Shutdown`] naming the provider that could not
    /// flush or stop; both are asked to stop either way.
    pub fn shutdown(&self) -> Result<(), InitError> {
        let traces = self
            .tracers
            .as_ref()
            .map(SdkTracerProvider::shutdown)
            .transpose()
            .map_err(|source| InitError::Shutdown {
                signal: "tracer",
                source,
            });
        let metrics = self
            .meters
            .shutdown()
            .map_err(|source| InitError::Shutdown {
                signal: "meter",
                source,
            });
        traces.and(metrics)
    }
}

/// How the console is drawn: whether stdout is a terminal, whether
/// `NO_COLOR` asks for no colour, and the directives no filter can drop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Console {
    terminal: bool,
    no_color: bool,
    forced: Option<Filter>,
}

impl Console {
    /// A console on a stdout that is a terminal when `stdout_is_terminal`
    /// holds, with `no_color` the value of `NO_COLOR`, which the caller
    /// passes so a test fixes both.
    #[must_use]
    pub fn new(stdout_is_terminal: bool, no_color: Option<&OsStr>) -> Self {
        Self {
            terminal: stdout_is_terminal,
            // NOTE: <https://no-color.org/>: a `NO_COLOR` that is present and not empty
            // turns colour off, whatever its value.
            no_color: no_color.is_some_and(|value| !value.is_empty()),
            forced: None,
        }
    }

    /// The console of this process: whether its stdout is a terminal, and
    /// its `NO_COLOR`.
    #[must_use]
    pub fn detect() -> Self {
        Self::new(
            io::stdout().is_terminal(),
            std::env::var_os("NO_COLOR").as_deref(),
        )
    }

    /// Writes what `directives` let through whatever the configured filter
    /// says, such as a product's security log at `info`
    /// (`ferrofed::security=info`).
    ///
    /// The directives are their own filter beside the configured one, and
    /// replacing the configured filter keeps them.
    #[must_use]
    pub fn force(mut self, directives: Filter) -> Self {
        self.forced = Some(directives);
        self
    }

    /// Whether the console writes colour for `format`: never under
    /// `NO_COLOR`; otherwise always for an explicit `pretty`, because a
    /// person asked for it, and on a terminal for `auto` and `json`.
    #[must_use]
    pub fn colour(&self, format: Format) -> bool {
        !self.no_color && (matches!(format, Format::Pretty) || self.terminal)
    }
}

/// The subscriber layers and the handle of the console filter.
pub struct Layers<S> {
    /// The layers, for `tracing_subscriber::registry().with(layers)`.
    pub layers: Vec<BoxedLayer<S>>,
    /// The handle that replaces the console filter.
    pub filter: FilterHandle,
}

impl<S> fmt::Debug for Layers<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Layers")
            .field("layers", &self.layers.len())
            .field("filter", &self.filter)
            .finish()
    }
}

/// The swap of the console filter.
type Swap = dyn Fn(EnvFilter) -> Result<(), reload::Error> + Send + Sync;

/// The read of the console filter.
type Current = dyn Fn() -> Option<String> + Send + Sync;

/// The handle that replaces the configured console filter at run time; the
/// forced directives stay.
#[derive(Clone)]
pub struct FilterHandle {
    swap: Arc<Swap>,
    current: Arc<Current>,
}

impl fmt::Debug for FilterHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilterHandle").finish_non_exhaustive()
    }
}

impl FilterHandle {
    /// The handle over the reload `handle` of the configured filter.
    fn new<S>(handle: reload::Handle<EnvFilter, S>) -> Self
    where
        S: 'static,
    {
        let read = handle.clone();
        Self {
            swap: Arc::new(move |filter| handle.reload(filter)),
            current: Arc::new(move || read.with_current(ToString::to_string).ok()),
        }
    }

    /// Replaces the configured console filter with `directive`.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Filter`] when the directive does not parse and
    /// [`InitError::Reload`] when the subscriber is gone; the running filter
    /// stays either way.
    pub fn replace(&self, directive: &str) -> Result<(), InitError> {
        let filter = Filter::new(directive)?.env_filter()?;
        (self.swap)(filter).map_err(|source| InitError::Reload { source })
    }

    /// Returns the configured console filter now running, or `None` when
    /// the subscriber is gone.
    #[must_use]
    pub fn current(&self) -> Option<String> {
        (self.current)()
    }
}

/// Returns the route that answers `GET` and `HEAD` with every metric of
/// `telemetry` in the Prometheus text exposition format.
///
/// The content type is `text/plain; version=0.0.4` (Prometheus, exposition
/// formats, text format). Mount it as
/// `.route("/metrics", prometheus_route(telemetry))`.
///
/// A rendering that fails answers `500` with no body and logs one line.
#[cfg(all(feature = "axum", feature = "prometheus"))]
pub fn prometheus_route<S>(telemetry: Arc<Telemetry>) -> axum::routing::MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    axum::routing::get(move || {
        let telemetry = Arc::clone(&telemetry);
        async move { exposition(&telemetry) }
    })
}

/// The answer of the Prometheus route.
#[cfg(all(feature = "axum", feature = "prometheus"))]
fn exposition(telemetry: &Telemetry) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    match telemetry.render_prometheus() {
        Ok(text) => (
            [(http::header::CONTENT_TYPE, prometheus::TEXT_FORMAT)],
            text,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "the metrics could not be rendered");
            http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Returns the sampler of the span export: a root span is sampled by its
/// trace id at `ratio`, and every other span as its parent was, so a
/// request's whole span tree is exported or none of it is.
#[must_use]
pub fn sampler(ratio: SampleRatio) -> Sampler {
    Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio.get())))
}

/// The OTLP export, when the build has it.
#[cfg(feature = "otlp")]
mod otlp {
    use opentelemetry_otlp::tonic_types::transport::{Certificate, ClientTlsConfig, Identity};
    use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::metrics::{MeterProviderBuilder, PeriodicReader, Temporality};
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use rustls::pki_types::pem::PemObject as _;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use secrecy::ExposeSecret as _;

    use super::{CLIENT_IDENTITY_KEY, InitError, TRUST_ROOTS_KEY, sampler};
    use crate::section::OtlpSection;

    /// Adds the metrics push to `meters` and builds the span export, each
    /// when `section` turns it on.
    pub(super) fn export(
        section: &OtlpSection,
        mut meters: MeterProviderBuilder,
        resource: Resource,
    ) -> Result<(MeterProviderBuilder, Option<SdkTracerProvider>), InitError> {
        let endpoint = section.endpoint.expose().to_owned();
        let tls = tls(section)?;
        if section.metrics {
            let mut builder = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone());
            if let Some(tls) = &tls {
                builder = builder.with_tls_config(tls.clone());
            }
            let exporter = builder
                .with_temporality(Temporality::Cumulative)
                .build()
                .map_err(|source| InitError::Exporter { source })?;
            meters = meters.with_reader(PeriodicReader::builder(exporter).build());
        }
        let tracers = if section.traces {
            let mut builder = opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint);
            if let Some(tls) = tls {
                builder = builder.with_tls_config(tls);
            }
            let exporter = builder
                .build()
                .map_err(|source| InitError::Exporter { source })?;
            Some(
                SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource)
                    .with_sampler(sampler(section.sample_ratio))
                    .build(),
            )
        } else {
            None
        };
        Ok((meters, tracers))
    }

    /// The TLS of an `https://` collector: the platform's trust roots, the
    /// ones `trust_roots_file` adds, and the client identity
    /// `client_identity_file` names for mutual TLS.
    fn tls(section: &OtlpSection) -> Result<Option<ClientTlsConfig>, InitError> {
        if !section.endpoint.expose().starts_with("https://") {
            return Ok(None);
        }
        // NOTE: tonic builds its TLS client from rustls's process-level provider; an
        // earlier install by the product wins, so the outcome of this one is dropped.
        let _installed: Result<(), std::sync::Arc<rustls::crypto::CryptoProvider>> =
            rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut config = ClientTlsConfig::new().with_native_roots();
        if let Some(path) = &section.trust_roots_file {
            let roots = std::fs::read(path).map_err(|source| InitError::TlsFile {
                key: TRUST_ROOTS_KEY,
                path: path.clone(),
                source,
            })?;
            if !holds_certificates(&roots) {
                return Err(pem_refusal(TRUST_ROOTS_KEY, path, "PEM certificate"));
            }
            config = config.ca_certificate(Certificate::from_pem(roots));
        }
        if let Some(path) = &section.client_identity_file {
            let pem =
                ferrohealth_secret::file::read_secret_file("telemetry.otlp.client_identity", path)?;
            let pem = pem.expose_secret();
            if !holds_certificates(pem.as_bytes()) {
                return Err(pem_refusal(CLIENT_IDENTITY_KEY, path, "PEM certificate"));
            }
            if PrivateKeyDer::from_pem_slice(pem.as_bytes()).is_err() {
                return Err(pem_refusal(CLIENT_IDENTITY_KEY, path, "PEM private key"));
            }
            config = config.identity(Identity::from_pem(pem, pem));
        }
        Ok(Some(config))
    }

    /// Whether `pem` holds one certificate or more, and every PEM section of
    /// a certificate in it parses.
    fn holds_certificates(pem: &[u8]) -> bool {
        let mut certificates = CertificateDer::pem_slice_iter(pem).peekable();
        certificates.peek().is_some() && certificates.all(|certificate| certificate.is_ok())
    }

    /// The refusal of the file `key` names at `path`, which lacks `wanted`.
    fn pem_refusal(key: &'static str, path: &std::path::Path, wanted: &'static str) -> InitError {
        InitError::TlsPem {
            key,
            path: path.to_path_buf(),
            wanted,
        }
    }
}

/// The stand-in of the OTLP export in a build without it.
#[cfg(not(feature = "otlp"))]
mod otlp {
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::metrics::MeterProviderBuilder;
    use opentelemetry_sdk::trace::SdkTracerProvider;

    use super::InitError;
    use crate::section::OtlpSection;

    /// Refuses `[telemetry.otlp]`: this build cannot honour it.
    pub(super) fn export(
        _section: &OtlpSection,
        _meters: MeterProviderBuilder,
        _resource: Resource,
    ) -> Result<(MeterProviderBuilder, Option<SdkTracerProvider>), InitError> {
        Err(InitError::NoOtlp)
    }
}

#[cfg(test)]
mod tests;
