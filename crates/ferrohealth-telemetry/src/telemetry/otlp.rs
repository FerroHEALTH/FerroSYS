// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The OTLP export of spans and metrics, and the dial of an `https://`
//! collector over the TLS configuration the product built, in a build
//! with the `otlp` feature.

use std::sync::Arc;

use http::Uri;
use opentelemetry_otlp::{WithExportConfig as _, WithTonicConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::{MeterProviderBuilder, PeriodicReader, Temporality};
use opentelemetry_sdk::trace::SdkTracerProvider;
use rustls::pki_types::ServerName;
use tonic::transport::{Channel, Endpoint};

use super::{InitError, sampler};
use crate::section::OtlpSection;

/// The TLS configuration the product handed, if any.
pub(super) type Handed = Option<Arc<rustls::ClientConfig>>;

/// Where the exporters send: a plain-HTTP endpoint tonic dials itself,
/// or a channel over the handed TLS.
enum Target {
    Plain(String),
    Tls(Channel),
}

/// Adds the metrics push to `meters` and builds the span export, each
/// when `section` turns it on.
pub(super) fn export(
    section: &OtlpSection,
    mut meters: MeterProviderBuilder,
    resource: Resource,
    tls: Handed,
) -> Result<(MeterProviderBuilder, Option<SdkTracerProvider>), InitError> {
    let endpoint = section.endpoint.expose().to_owned();
    let target = if endpoint.starts_with("https://") {
        let tls = tls.ok_or(InitError::TlsNotHanded)?;
        Target::Tls(channel(&endpoint, &tls)?)
    } else {
        Target::Plain(endpoint)
    };
    if section.metrics {
        let builder = opentelemetry_otlp::MetricExporter::builder().with_tonic();
        let builder = match &target {
            Target::Plain(endpoint) => builder.with_endpoint(endpoint.clone()),
            Target::Tls(channel) => builder.with_channel(channel.clone()),
        };
        let exporter = builder
            .with_temporality(Temporality::Cumulative)
            .build()
            .map_err(|source| InitError::Exporter { source })?;
        meters = meters.with_reader(PeriodicReader::builder(exporter).build());
    }
    let tracers = if section.traces {
        let builder = opentelemetry_otlp::SpanExporter::builder().with_tonic();
        let builder = match target {
            Target::Plain(endpoint) => builder.with_endpoint(endpoint),
            Target::Tls(channel) => builder.with_channel(channel),
        };
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

/// The channel to the `https://` collector at `endpoint`, dialed over
/// `tls` with the ALPN protocol `h2` (RFC 9113 §3.2), connecting on the
/// first export.
fn channel(endpoint: &str, tls: &rustls::ClientConfig) -> Result<Channel, InitError> {
    let target = Endpoint::from_shared(endpoint.to_owned())
        .map_err(|source| InitError::Endpoint { source })?;
    let dial = Dial::new(target.uri(), tls)?;
    // NOTE: no specification governs this: our own design; the crate links tonic without
    // its TLS features, so tonic passes the stream the dial returns through as it is.
    Ok(
        target.connect_with_connector_lazy(tower::service_fn(move |_uri: Uri| {
            let dial = dial.clone();
            async move { dial.connect().await }
        })),
    )
}

/// The TCP and TLS dial of one collector.
#[derive(Clone)]
struct Dial {
    host: String,
    port: u16,
    server_name: ServerName<'static>,
    connector: tokio_rustls::TlsConnector,
}

impl Dial {
    /// The dial of the `https` URI `uri` over `tls`.
    fn new(uri: &Uri, tls: &rustls::ClientConfig) -> Result<Self, InitError> {
        let host = uri.host().ok_or(InitError::ServerName)?;
        let host = host
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host)
            .to_owned();
        let server_name =
            ServerName::try_from(host.clone()).map_err(|_invalid| InitError::ServerName)?;
        let mut config = tls.clone();
        config.alpn_protocols = vec![b"h2".to_vec()];
        Ok(Self {
            host,
            // NOTE: RFC 9110 §4.2.2, an https URI without a port names port 443.
            port: uri.port_u16().unwrap_or(443),
            server_name,
            connector: tokio_rustls::TlsConnector::from(Arc::new(config)),
        })
    }

    /// Connects to the collector and completes the TLS handshake.
    async fn connect(
        &self,
    ) -> std::io::Result<
        hyper_util::rt::TokioIo<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    > {
        let tcp = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await?;
        let stream = self
            .connector
            .connect(self.server_name.clone(), tcp)
            .await?;
        Ok(hyper_util::rt::TokioIo::new(stream))
    }
}

#[cfg(test)]
mod tests;
