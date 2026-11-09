// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the telemetry: the filter, the console, the export and the
//! resource.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex};

use ferrohealth_secret::secret_url::SecretUrl;
use tracing_subscriber::layer::SubscriberExt as _;

use super::{Console, InitError, Service, Telemetry};
use crate::section::{Filter, Format, OtlpSection, TelemetrySection};

/// A writer that keeps what it is given.
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

fn telemetry() -> Telemetry {
    Telemetry::new(
        &TelemetrySection::default(),
        Service::new("ferrosys", "0.0.0"),
    )
    .expect("the telemetry builds")
}

fn section(format: Format, filter: &str) -> TelemetrySection {
    TelemetrySection {
        format,
        filter: Filter::new(filter).expect("the filter parses"),
        ..TelemetrySection::default()
    }
}

#[test]
fn no_color_set_and_not_empty_turns_colour_off() {
    let off = Some(OsStr::new("1"));
    assert!(!Console::new(true, off).colour(Format::Pretty));
    assert!(!Console::new(true, off).colour(Format::Auto));
    assert!(!Console::new(false, Some(OsStr::new("false"))).colour(Format::Pretty));
    let empty = Some(OsStr::new(""));
    assert!(
        Console::new(true, empty).colour(Format::Auto),
        "an empty NO_COLOR is unset"
    );
    assert!(
        Console::new(false, None).colour(Format::Pretty),
        "pretty asked for colour"
    );
    assert!(!Console::new(false, None).colour(Format::Auto));
}

#[test]
fn the_pretty_console_under_no_color_writes_no_escape() {
    let draw = |console: &Console| {
        let capture = Capture::default();
        let layers = telemetry()
            .layers(&section(Format::Pretty, "info"), console, capture.clone())
            .expect("the layers build")
            .layers;
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layers), || {
            tracing::info!("drawn");
        });
        capture.text()
    };
    assert!(
        draw(&Console::new(true, None)).contains('\u{1b}'),
        "colour by default"
    );
    let plain = draw(&Console::new(true, Some(OsStr::new("1"))));
    assert!(
        plain.contains("drawn") && !plain.contains('\u{1b}'),
        "{plain}"
    );
}

#[test]
fn a_forced_target_logs_under_an_error_only_filter() {
    let capture = Capture::default();
    let console =
        Console::new(false, None).force(Filter::new("ferrosys::security=info").expect("parses"));
    let layers = telemetry()
        .layers(&section(Format::Json, "error"), &console, capture.clone())
        .expect("the layers build")
        .layers;
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layers), || {
        tracing::info!(target: "ferrosys::security", "Qz7forced");
        tracing::debug!(target: "ferrosys::security", "Qz7below");
        tracing::info!(target: "ferrosys::other", "Qz7quiet");
        tracing::error!(target: "ferrosys::other", "Qz7error");
    });
    let text = capture.text();
    assert!(text.contains("Qz7forced"), "{text}");
    assert!(text.contains("Qz7error"), "{text}");
    assert!(!text.contains("Qz7below"), "the forced level holds: {text}");
    assert!(!text.contains("Qz7quiet"), "{text}");
}

#[test]
fn the_handle_swaps_the_filter_keeps_the_forced_directives_and_refuses_a_bad_one() {
    let capture = Capture::default();
    let console =
        Console::new(false, None).force(Filter::new("ferrosys::security=info").expect("parses"));
    let layers = telemetry()
        .layers(&section(Format::Json, "error"), &console, capture.clone())
        .expect("the layers build");
    let handle = layers.filter.clone();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layers.layers), || {
        tracing::info!(target: "ferrosys::other", "Qz7before");
        assert_eq!(Some("error".to_owned()), handle.current());
        handle.replace("info").expect("the filter swaps");
        assert_eq!(Some("info".to_owned()), handle.current());
        tracing::info!(target: "ferrosys::other", "Qz7after");
        let refused = handle
            .replace("info,=,,")
            .expect_err("the directive does not parse");
        assert!(matches!(refused, InitError::Filter(_)), "{refused:?}");
        assert_eq!(
            Some("info".to_owned()),
            handle.current(),
            "the running filter stays"
        );
        handle.replace("off").expect("the filter swaps");
        tracing::info!(target: "ferrosys::other", "Qz7silenced");
        tracing::info!(target: "ferrosys::security", "Qz7forced");
    });
    let text = capture.text();
    assert!(!text.contains("Qz7before"), "{text}");
    assert!(text.contains("Qz7after"), "{text}");
    assert!(!text.contains("Qz7silenced"), "{text}");
    assert!(
        text.contains("Qz7forced"),
        "a swap keeps the forced directives: {text}"
    );
}

#[test]
fn the_environment_of_the_section_names_the_deployment_on_the_resource() {
    let section = TelemetrySection {
        environment: Some("production".to_owned()),
        ..TelemetrySection::default()
    };
    let telemetry =
        Telemetry::new(&section, Service::new("ferrosys", "0.0.0")).expect("the telemetry builds");
    let resource = telemetry.service.resource();
    assert_eq!(
        Some("production".to_owned()),
        resource
            .get(&opentelemetry::Key::from_static_str(
                "deployment.environment.name"
            ))
            .map(|value| value.to_string())
    );
    let unnamed = Service::new("ferrosys", "0.0.0").resource();
    assert!(
        unnamed
            .get(&opentelemetry::Key::from_static_str(
                "deployment.environment.name"
            ))
            .is_none(),
        "no environment, no attribute"
    );
}

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

#[test]
fn tls_material_beside_a_plain_http_collector_is_refused() {
    for (roots, identity) in [(true, false), (false, true)] {
        let mut section = with_otlp("http://127.0.0.1:4317");
        if let Some(otlp) = section.otlp.as_mut() {
            otlp.tls.trust_roots_file = roots.then(|| "/run/roots.pem".into());
            otlp.tls.client_identity_file = identity.then(|| "/run/identity.pem".into());
        }
        let error = Telemetry::new(&section, Service::new("ferrosys", "0.0.0"))
            .expect_err("the material would be ignored");
        assert!(matches!(&error, InitError::TlsWithoutHttps), "{error:?}");
        assert!(
            error.to_string().starts_with("telemetry.otlp.tls"),
            "{error}"
        );
    }
}

#[cfg(feature = "otlp")]
#[tokio::test]
async fn an_https_collector_without_handed_tls_is_refused() {
    let error = Telemetry::new(
        &with_otlp("https://otel.example.org:4317"),
        Service::new("ferrosys", "0.0.0"),
    )
    .expect_err("no TLS was handed");
    assert!(matches!(error, InitError::TlsNotHanded), "{error:?}");
}

#[cfg(feature = "otlp")]
#[tokio::test]
async fn an_https_collector_builds_both_exports_over_the_handed_tls() {
    let _installed = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let telemetry = Telemetry::with_tls(
        &with_otlp("https://otel.example.org:4317"),
        Service::new("ferrosys", "0.0.0"),
        Arc::new(tls),
    )
    .expect("the exporters build without connecting");
    assert!(telemetry.tracer().is_some(), "spans are exported");
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
