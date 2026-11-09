// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The telemetry of a product, with its OTLP collector dialed over the TLS
//! that [`TlsClient`] reads.
//!
//! `ferrohealth-telemetry` reads no TLS file, so an `https://` collector
//! needs a rustls configuration handed to `Telemetry::with_tls`. [`build`]
//! is that hand-over, written once for every product binary: it reads
//! `telemetry.otlp.tls` with [`TlsClient`] over the platform's verifier and
//! the table's extra roots, so the trust roots, the client identity and the
//! minimum version hold. No specification governs the hand-over: our own
//! design.

use std::sync::Arc;

use ferrohealth_telemetry::section::TelemetrySection;
use ferrohealth_telemetry::telemetry::{InitError, Service, Telemetry};

use crate::tls_client::{TlsClient, TlsClientError};

/// The dotted key of the collector's TLS table.
const OTLP_TLS: &str = "telemetry.otlp.tls";

/// The telemetry could not be built.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TelemetryBuildError {
    /// `telemetry.otlp.tls` could not be read into a TLS configuration.
    #[error("telemetry.otlp.tls is refused")]
    Tls(#[source] TlsClientError),
    /// The providers could not be built.
    #[error("the telemetry could not be built")]
    Init(#[source] InitError),
}

/// Builds the telemetry `section` asks for, described by `service`.
///
/// With `[telemetry.otlp]` the collector's TLS is read from
/// `telemetry.otlp.tls` and handed to `Telemetry::with_tls`, which dials
/// an `https://` collector over it and leaves a plain-HTTP one without it;
/// with no `[telemetry.otlp]` no TLS is read. With `[telemetry.otlp]` this
/// runs inside the Tokio runtime that carries the export.
///
/// # Errors
///
/// Returns [`TelemetryBuildError::Tls`] when `telemetry.otlp.tls` names a
/// file that cannot be read or material rustls refuses, and
/// [`TelemetryBuildError::Init`] with every error of `Telemetry::with_tls`
/// and `Telemetry::new`.
pub fn build(
    section: &TelemetrySection,
    service: Service,
) -> Result<Telemetry, TelemetryBuildError> {
    let Some(otlp) = &section.otlp else {
        return Telemetry::new(section, service).map_err(TelemetryBuildError::Init);
    };
    let tls = TlsClient::read(OTLP_TLS, &otlp.tls)
        .and_then(|client| client.platform_config())
        .map_err(TelemetryBuildError::Tls)?;
    Telemetry::with_tls(section, service, Arc::new(tls)).map_err(TelemetryBuildError::Init)
}

#[cfg(test)]
mod tests {
    use ferrohealth_secret::secret_url::SecretUrl;
    use ferrohealth_telemetry::section::{OtlpSection, TelemetrySection};
    use ferrohealth_telemetry::telemetry::{InitError, Service};

    use super::{TelemetryBuildError, build};

    fn with_otlp(endpoint: &str) -> TelemetrySection {
        TelemetrySection {
            otlp: Some(OtlpSection {
                endpoint: SecretUrl::new(endpoint),
                ..OtlpSection::default()
            }),
            ..TelemetrySection::default()
        }
    }

    fn service() -> Service {
        Service::new("ferrosys", "0.0.0")
    }

    #[test]
    fn no_collector_builds_with_no_tls() {
        let telemetry = build(&TelemetrySection::default(), service()).expect("it builds");
        assert!(telemetry.tracer().is_none(), "no spans are exported");
    }

    #[tokio::test]
    async fn a_plain_collector_builds_without_using_the_tls() {
        let telemetry = build(&with_otlp("http://127.0.0.1:4317"), service()).expect("it builds");
        assert!(telemetry.tracer().is_some(), "spans are exported");
    }

    #[tokio::test]
    async fn an_https_collector_builds_over_the_platform_verifier() {
        let telemetry =
            build(&with_otlp("https://otel.example.org:4317"), service()).expect("it builds");
        assert!(telemetry.tracer().is_some(), "spans are exported");
    }

    #[tokio::test]
    async fn an_unreadable_tls_file_is_refused_naming_the_table() {
        let mut section = with_otlp("https://otel.example.org:4317");
        if let Some(otlp) = section.otlp.as_mut() {
            otlp.tls.trust_roots_file = Some("/nonexistent/roots.pem".into());
        }
        let refusal = build(&section, service()).expect_err("the file is missing");
        assert!(matches!(refusal, TelemetryBuildError::Tls(_)), "{refusal}");
        let source = std::error::Error::source(&refusal)
            .expect("the cause")
            .to_string();
        assert!(source.contains("telemetry.otlp.tls"), "{source}");
    }

    #[tokio::test]
    async fn a_telemetry_refusal_carries_its_cause() {
        let refusal = build(&with_otlp(""), service()).expect_err("no endpoint");
        assert!(
            matches!(
                refusal,
                TelemetryBuildError::Init(InitError::MissingEndpoint)
            ),
            "{refusal}"
        );
    }
}
