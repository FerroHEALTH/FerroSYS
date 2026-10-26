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

use std::fmt;
use std::io;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, MeterProvider as _};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};
use tracing::{Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::FilterFn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::console::{self, BoxedLayer};
use crate::section::{FilterError, Format, SampleRatio, TelemetrySection};

/// The service the telemetry describes: the OpenTelemetry resource of every
/// span and metric, and the crates whose spans are exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    name: String,
    version: String,
    instance_id: Option<String>,
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
        }
    }

    /// Adds the installation's id as `service.instance.id`.
    #[must_use]
    pub fn instance_id(mut self, id: impl Into<String>) -> Self {
        self.instance_id = Some(id.into());
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
    /// [`InitError::TlsFile`] and [`InitError::Identity`] for an OTLP table
    /// this build cannot honour, `InitError::Exporter` when an exporter
    /// cannot be built, and [`InitError::Prometheus`] when the Prometheus
    /// reader cannot be built.
    pub fn new(section: &TelemetrySection, service: Service) -> Result<Self, InitError> {
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

    /// Returns the subscriber layers: the console for `format` with
    /// `section`'s filter, writing through `writer`, and the span export.
    ///
    /// The filter decides what the console writes and nothing else, so a
    /// quieter log never thins a trace.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Filter`] when the filter does not parse, which a
    /// section read through `ferrohealth-config` never holds.
    pub fn layers<S, W>(
        &self,
        section: &TelemetrySection,
        stdout_is_terminal: bool,
        writer: W,
    ) -> Result<Vec<BoxedLayer<S>>, InitError>
    where
        S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync,
        W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
    {
        let rendering = section.format.resolve(stdout_is_terminal);
        let ansi = matches!(section.format, Format::Pretty) || stdout_is_terminal;
        let filter = section.filter.env_filter()?;
        let mut layers = vec![
            console::layer(rendering, ansi, writer)
                .with_filter(filter)
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
        Ok(layers)
    }

    /// Installs the process-wide subscriber on stdout.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Filter`] when the filter does not parse and
    /// [`InitError::AlreadyInstalled`] when the process has a subscriber.
    pub fn install(
        &self,
        section: &TelemetrySection,
        stdout_is_terminal: bool,
    ) -> Result<(), InitError> {
        let layers = self.layers(section, stdout_is_terminal, io::stdout)?;
        tracing_subscriber::registry()
            .with(layers)
            .try_init()
            .map_err(|source| InitError::AlreadyInstalled { source })
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
    use secrecy::ExposeSecret as _;

    use super::{InitError, sampler};
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
                key: "telemetry.otlp.trust_roots_file",
                path: path.clone(),
                source,
            })?;
            config = config.ca_certificate(Certificate::from_pem(roots));
        }
        if let Some(path) = &section.client_identity_file {
            let pem =
                ferrohealth_secret::file::read_secret_file("telemetry.otlp.client_identity", path)?;
            let pem = pem.expose_secret();
            config = config.identity(Identity::from_pem(pem, pem));
        }
        Ok(Some(config))
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
mod tests {
    use ferrohealth_secret::secret_url::SecretUrl;

    use super::{InitError, Service, Telemetry};
    use crate::section::{OtlpSection, TelemetrySection};

    fn with_otlp(endpoint: &str) -> TelemetrySection {
        TelemetrySection {
            otlp: Some(OtlpSection {
                endpoint: SecretUrl::new(endpoint),
                ..OtlpSection::default()
            }),
            ..TelemetrySection::default()
        }
    }

    #[test]
    fn the_resource_names_the_service_its_version_and_its_instance() {
        let resource = Service::new("ferrosys", "1.2.3")
            .instance_id("4f0c")
            .resource();
        let get = |key: &'static str| {
            resource
                .get(&opentelemetry::Key::from_static_str(key))
                .map(|value| value.to_string())
        };
        assert_eq!(Some("ferrosys".to_owned()), get("service.name"));
        assert_eq!(Some("1.2.3".to_owned()), get("service.version"));
        assert_eq!(Some("4f0c".to_owned()), get("service.instance.id"));
    }

    #[test]
    fn a_section_without_otlp_exports_nothing() {
        let telemetry = Telemetry::new(
            &TelemetrySection::default(),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect("the telemetry builds");
        assert!(telemetry.tracer().is_none(), "no span export");
        telemetry.shutdown().expect("the providers stop");
    }

    #[test]
    fn an_otlp_table_without_an_endpoint_is_refused() {
        let error = Telemetry::new(&with_otlp(""), Service::new("ferrosys", "0.0.0"))
            .expect_err("the endpoint is required");
        assert!(matches!(error, InitError::MissingEndpoint), "{error:?}");
    }

    #[cfg(not(feature = "otlp"))]
    #[test]
    fn a_build_without_otlp_refuses_the_table() {
        let error = Telemetry::new(
            &with_otlp("http://127.0.0.1:4317"),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect_err("the build has no OTLP export");
        assert!(matches!(error, InitError::NoOtlp), "{error:?}");
    }

    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn an_otlp_table_builds_both_exports() {
        let telemetry = Telemetry::new(
            &with_otlp("http://127.0.0.1:4317"),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect("the exporters build without connecting");
        assert!(telemetry.tracer().is_some(), "spans are exported");
    }

    #[cfg(feature = "otlp")]
    #[tokio::test]
    async fn a_trust_roots_file_that_cannot_be_read_is_refused() {
        let mut section = with_otlp("https://otel.example.org:4317");
        if let Some(otlp) = section.otlp.as_mut() {
            otlp.trust_roots_file = Some("/nonexistent/roots.pem".into());
        }
        let error = Telemetry::new(&section, Service::new("ferrosys", "0.0.0"))
            .expect_err("the file is missing");
        assert!(
            matches!(&error, InitError::TlsFile { key, .. } if *key == "telemetry.otlp.trust_roots_file"),
            "{error:?}"
        );
    }

    #[cfg(feature = "prometheus")]
    #[test]
    fn an_instrument_of_the_one_provider_reaches_the_prometheus_text() {
        let telemetry = Telemetry::new(
            &TelemetrySection::default(),
            Service::new("ferrosys", "0.0.0"),
        )
        .expect("the telemetry builds");

        let requests = telemetry
            .meter("ferrosys")
            .u64_counter("ferrosys.requests")
            .build();
        requests.add(3, &[opentelemetry::KeyValue::new("route", "/health")]);
        let text = telemetry.render_prometheus().expect("the metrics render");
        assert!(
            text.contains("ferrosys_requests_total{route=\"/health\"} 3"),
            "{text}"
        );
    }

    #[test]
    fn only_spans_of_the_service_crates_and_the_request_span_are_exported() {
        let service = Service::new("ferrosys", "0.0.0").export_crate("ferrohealth_smart");
        let exports = |target: &str, span: bool| service.exports_target(target, span);
        assert!(exports("ferrosys_server::facade", true));
        assert!(exports("ferrohealth_smart::audit", true));
        assert!(exports("ferrohealth_telemetry::request", true));
        assert!(
            !exports("ferrosys_server::facade", false),
            "an event never leaves"
        );
        assert!(
            !exports("hyper::client", true),
            "another crate's span never leaves"
        );
    }
}
