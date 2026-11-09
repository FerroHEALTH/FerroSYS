// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `[server]` section of a product's configuration.
//!
//! Every span of time is a `ferrohealth_config::units::Duration` and every
//! amount of data a `ByteSize`, so a key names no unit
//! (`request_timeout = "30s"`). The keys of 0.6, which spelled the unit into
//! the name (`request_timeout_ms`), are refused at load naming their
//! replacement.

use std::net::SocketAddr;
use std::num::NonZeroUsize;

use ferrohealth_config::tls::TlsVersion;
use ferrohealth_config::units::{ByteSize, Duration};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::client_address::Forwarding;

/// The `[server]` section.
///
/// Its `Default` is the crate's. A product states its own defaults for the
/// section with `ferrohealth_config::loader::Loader::defaults`, which layers
/// them under the operator's file, environment and `--set` pairs, so a
/// partial `[server]` table keeps the product's value for every key it does
/// not name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "RawServer")]
#[cfg_attr(feature = "schema", schemars(!try_from, default, deny_unknown_fields))]
pub struct ServerSection {
    /// The socket address the listener binds.
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub listen: SocketAddr,
    /// The path every route is served under: empty, or starting with `/`
    /// and not ending with one.
    pub base_path: BasePath,
    /// How long one request may take before the server answers `408`.
    pub request_timeout: Duration,
    /// How long a client may take to send its first byte once it connected,
    /// and each HTTP/1.1 request head from that head's first byte; a client
    /// still sending its head then is disconnected, which bounds a
    /// connection that never finishes one. Never zero.
    pub header_read_timeout: Duration,
    /// How long a kept-alive HTTP/1.1 connection may wait for its next
    /// request before the server closes it. Keep it longer than the idle
    /// timeout of the clients and proxies in front, so they close an idle
    /// connection first. Never zero.
    pub idle_timeout: Duration,
    /// The largest request body the server reads before answering `413`.
    pub body_limit: ByteSize,
    /// How long readiness stays withdrawn before the listener closes, so an
    /// orchestrator stops sending requests first.
    pub drain_delay: Duration,
    /// How long the requests in flight may take once the listener closed.
    pub shutdown_timeout: Duration,
    /// How many requests the server serves at once; past it a request is
    /// answered `503`. Unset, there is no bound.
    #[cfg_attr(feature = "schema", schemars(with = "Option<u64>"))]
    pub max_concurrent_requests: Option<NonZeroUsize>,
    /// How long a refused request is told to wait (`Retry-After`), a whole
    /// number of seconds, since the header carries delay-seconds (RFC 9110
    /// §10.2.3).
    pub retry_after: Duration,
    /// The listener's TLS; unset, the listener speaks plain HTTP behind a
    /// proxy that terminates TLS.
    pub tls: Option<TlsSection>,
    /// The reverse proxies whose forwarded client address is believed, each
    /// an address or a CIDR block; empty, every request is named by its peer.
    pub trusted_proxies: Vec<crate::client_address::TrustedProxy>,
    /// The header a trusted proxy names the client in.
    pub forwarded_header: crate::client_address::ForwardedHeader,
    /// The requests one caller may send; unset, a caller is not limited.
    pub caller_rate: Option<CallerRate>,
    /// Whether a response is compressed with gzip or Brotli when the client
    /// accepts one.
    pub compression: bool,
    /// The smallest response body that is compressed.
    pub compression_min_size: ByteSize,
}

/// The `[server.caller_rate]` table: the steady rate and the burst one caller
/// may send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CallerRate {
    /// The requests per second a caller may send once its burst is spent.
    #[cfg_attr(feature = "schema", schemars(with = "u32", range(min = 1)))]
    pub requests_per_second: std::num::NonZeroU32,
    /// The requests a caller may send at once.
    #[cfg_attr(feature = "schema", schemars(with = "u32", range(min = 1)))]
    pub burst: std::num::NonZeroU32,
}

/// The `[server.tls]` table: the certificate chain and the private key the
/// listener serves, each in PEM, and the client certificates it asks for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "RawTls")]
#[cfg_attr(feature = "schema", schemars(!try_from, deny_unknown_fields))]
pub struct TlsSection {
    /// The certificate chain, the leaf first.
    pub certificate_file: std::path::PathBuf,
    /// The private key of the leaf.
    pub key_file: std::path::PathBuf,
    /// Whether the listener asks a client certificate.
    #[cfg_attr(feature = "schema", schemars(default))]
    pub client_auth: ClientAuth,
    /// The CA bundle a client certificate must chain to; required unless
    /// `client_auth` is `none`, and refused when it is.
    pub client_ca_file: Option<std::path::PathBuf>,
    /// How often the files are read again, never zero; unset, only `SIGHUP`
    /// reads them again.
    pub reload_interval: Option<Duration>,
    /// The client certificate chain and key, in one PEM, the healthcheck
    /// presents to a listener that asks one.
    pub healthcheck_identity_file: Option<std::path::PathBuf>,
    /// The oldest TLS version the listener speaks: `"1.3"` unless lowered to
    /// `"1.2"`.
    #[cfg_attr(feature = "schema", schemars(default))]
    pub min_version: TlsVersion,
}

/// Whether the listener asks a client certificate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ClientAuth {
    /// No client certificate is asked.
    #[default]
    None,
    /// A client certificate is verified when presented.
    Optional,
    /// A verified client certificate is required.
    Required,
}

/// The `[server]` table as written, checked into a [`ServerSection`].
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServer {
    listen: SocketAddr,
    base_path: BasePath,
    request_timeout: Duration,
    header_read_timeout: Duration,
    idle_timeout: Duration,
    body_limit: ByteSize,
    drain_delay: Duration,
    shutdown_timeout: Duration,
    max_concurrent_requests: Option<NonZeroUsize>,
    retry_after: Duration,
    tls: Option<TlsSection>,
    trusted_proxies: Vec<crate::client_address::TrustedProxy>,
    forwarded_header: crate::client_address::ForwardedHeader,
    caller_rate: Option<CallerRate>,
    compression: bool,
    compression_min_size: ByteSize,
}

impl Default for RawServer {
    fn default() -> Self {
        let section = ServerSection::default();
        Self {
            listen: section.listen,
            base_path: section.base_path,
            request_timeout: section.request_timeout,
            header_read_timeout: section.header_read_timeout,
            idle_timeout: section.idle_timeout,
            body_limit: section.body_limit,
            drain_delay: section.drain_delay,
            shutdown_timeout: section.shutdown_timeout,
            max_concurrent_requests: section.max_concurrent_requests,
            retry_after: section.retry_after,
            tls: section.tls,
            trusted_proxies: section.trusted_proxies,
            forwarded_header: section.forwarded_header,
            caller_rate: section.caller_rate,
            compression: section.compression,
            compression_min_size: section.compression_min_size,
        }
    }
}

/// A `[server]` table the server refuses to start on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerSectionError {
    /// A bound that must hold some time is zero.
    #[error("server.{key} is zero; it bounds a connection, so give it a duration such as `30s`")]
    Zero {
        /// The key, under `server`.
        key: &'static str,
    },
    /// `retry_after` is not a whole number of seconds.
    #[error(
        "server.retry_after is sent as whole seconds (RFC 9110 §10.2.3); give it in seconds, such as `1s`"
    )]
    RetryAfterSeconds,
    /// `body_limit` is larger than this platform can address.
    #[error("server.body_limit is larger than this platform can address")]
    BodyLimit,
}

impl TryFrom<RawServer> for ServerSection {
    type Error = ServerSectionError;

    fn try_from(raw: RawServer) -> Result<Self, Self::Error> {
        for (key, bound) in [
            ("header_read_timeout", raw.header_read_timeout),
            ("idle_timeout", raw.idle_timeout),
        ] {
            if bound.is_zero() {
                return Err(ServerSectionError::Zero { key });
            }
        }
        if !raw.retry_after.is_whole_seconds() {
            return Err(ServerSectionError::RetryAfterSeconds);
        }
        if usize::try_from(raw.body_limit).is_err() {
            return Err(ServerSectionError::BodyLimit);
        }
        Ok(Self {
            listen: raw.listen,
            base_path: raw.base_path,
            request_timeout: raw.request_timeout,
            header_read_timeout: raw.header_read_timeout,
            idle_timeout: raw.idle_timeout,
            body_limit: raw.body_limit,
            drain_delay: raw.drain_delay,
            shutdown_timeout: raw.shutdown_timeout,
            max_concurrent_requests: raw.max_concurrent_requests,
            retry_after: raw.retry_after,
            tls: raw.tls,
            trusted_proxies: raw.trusted_proxies,
            forwarded_header: raw.forwarded_header,
            caller_rate: raw.caller_rate,
            compression: raw.compression,
            compression_min_size: raw.compression_min_size,
        })
    }
}

/// The `[server.tls]` table as written, checked into a [`TlsSection`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTls {
    certificate_file: std::path::PathBuf,
    key_file: std::path::PathBuf,
    #[serde(default)]
    client_auth: ClientAuth,
    client_ca_file: Option<std::path::PathBuf>,
    reload_interval: Option<Duration>,
    healthcheck_identity_file: Option<std::path::PathBuf>,
    #[serde(default)]
    min_version: TlsVersion,
}

/// A `[server.tls]` table whose `client_auth` names no CA.
#[derive(Debug, thiserror::Error)]
#[error("client_auth asks a client certificate, and client_ca_file names no CA it must chain to")]
pub struct NoClientCa;

/// A `[server.tls]` table the listener refuses.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientAuthError {
    /// `client_auth` asks a client certificate and no CA is named.
    #[error(transparent)]
    NoClientCa(#[from] NoClientCa),
    /// `client_ca_file` is set while `client_auth` is `none`, the default, so
    /// the listener would ask no client certificate and the CA would do
    /// nothing.
    #[error(
        "client_ca_file is set while client_auth is none (the default), so the listener would \
         ask no client certificate; set client_auth to required or optional, or remove \
         client_ca_file"
    )]
    UnusedClientCa,
    /// `reload_interval` is zero.
    #[error(
        "server.tls.reload_interval is zero; give it a duration such as `1h`, or leave it unset"
    )]
    ZeroReloadInterval,
}

impl TryFrom<RawTls> for TlsSection {
    type Error = ClientAuthError;

    fn try_from(raw: RawTls) -> Result<Self, Self::Error> {
        if raw.client_auth != ClientAuth::None && raw.client_ca_file.is_none() {
            return Err(NoClientCa.into());
        }
        // NOTE: RFC 8446 §4.3.2, a client sends a certificate only after a CertificateRequest,
        // which a listener under `none` never sends, so the CA could never take effect.
        if raw.client_auth == ClientAuth::None && raw.client_ca_file.is_some() {
            return Err(ClientAuthError::UnusedClientCa);
        }
        if raw.reload_interval.is_some_and(Duration::is_zero) {
            return Err(ClientAuthError::ZeroReloadInterval);
        }
        Ok(Self {
            certificate_file: raw.certificate_file,
            key_file: raw.key_file,
            client_auth: raw.client_auth,
            client_ca_file: raw.client_ca_file,
            reload_interval: raw.reload_interval,
            healthcheck_identity_file: raw.healthcheck_identity_file,
            min_version: raw.min_version,
        })
    }
}

impl TlsSection {
    /// The table of `certificate_file` and `key_file` alone: no client
    /// certificate, no reload interval.
    #[must_use]
    pub const fn new(certificate_file: std::path::PathBuf, key_file: std::path::PathBuf) -> Self {
        Self {
            certificate_file,
            key_file,
            client_auth: ClientAuth::None,
            client_ca_file: None,
            reload_interval: None,
            healthcheck_identity_file: None,
            min_version: TlsVersion::Tls13,
        }
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            base_path: BasePath::default(),
            request_timeout: Duration::from_secs(30),
            header_read_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_mins(2),
            body_limit: ByteSize::from_mib(1),
            drain_delay: Duration::ZERO,
            shutdown_timeout: Duration::from_secs(10),
            max_concurrent_requests: None,
            retry_after: Duration::from_secs(1),
            tls: None,
            trusted_proxies: Vec::new(),
            forwarded_header: crate::client_address::ForwardedHeader::default(),
            caller_rate: None,
            compression: true,
            compression_min_size: ByteSize::from_kib(1),
        }
    }
}

// NOTE: no specification governs this: our own design; the section reads its own
// keys, so it builds the forwarding they configure and client_address reads no section.
impl Forwarding {
    /// The forwarding `section` configures.
    #[must_use]
    pub fn from_section(section: &ServerSection) -> Self {
        Self::new(section.trusted_proxies.clone(), section.forwarded_header)
    }
}

impl ServerSection {
    /// The body limit in bytes, which the section checked fits the
    /// platform when it was read.
    #[must_use]
    pub fn body_limit_bytes(&self) -> usize {
        // NOTE: no specification governs this: our own design; a limit past the address
        // space bounds nothing more than the address space does, so it saturates.
        usize::try_from(self.body_limit).unwrap_or(usize::MAX)
    }
}

/// The path every route is served under, checked when the configuration is
/// read: empty, or `/` followed by segments with no trailing `/`.
///
/// # Examples
///
/// ```
/// use ferrohealth_server::section::BasePath;
///
/// assert_eq!("/ferropix", BasePath::new("/ferropix")?.as_str());
/// assert_eq!("/ferropix/health", BasePath::new("/ferropix")?.join("/health"));
/// assert!(BasePath::new("ferropix").is_err());
/// assert!(BasePath::new("/ferropix/").is_err());
/// # Ok::<(), ferrohealth_server::section::BasePathError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "schema",
    derive(schemars::JsonSchema),
    schemars(
        with = "String",
        description = "Empty, or / followed by path segments with no trailing / (RFC 3986 §3.3)."
    )
)]
pub struct BasePath(String);

/// A base path outside the form.
#[derive(Debug, thiserror::Error)]
#[error("the base path is not empty, or `/` followed by segments with no trailing `/`")]
pub struct BasePathError;

impl BasePath {
    /// Returns `path` as a base path.
    ///
    /// # Errors
    ///
    /// Returns [`BasePathError`] unless `path` is empty or starts with `/`,
    /// does not end with `/`, holds no empty segment and only the characters
    /// a path segment allows unescaped (RFC 3986 §3.3).
    pub fn new(path: impl Into<String>) -> Result<Self, BasePathError> {
        let path = path.into();
        if path.is_empty() {
            return Ok(Self(path));
        }
        let Some(rest) = path.strip_prefix('/') else {
            return Err(BasePathError);
        };
        let segments_are_valid = rest.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@".contains(c))
        });
        if segments_are_valid {
            Ok(Self(path))
        } else {
            Err(BasePathError)
        }
    }

    /// Returns the path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `route` under the base path.
    #[must_use]
    pub fn join(&self, route: &str) -> String {
        format!("{}{route}", self.0)
    }
}

impl<'de> Deserialize<'de> for BasePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let path = String::deserialize(deserializer)?;
        Self::new(path).map_err(serde::de::Error::custom)
    }
}

impl Serialize for BasePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
