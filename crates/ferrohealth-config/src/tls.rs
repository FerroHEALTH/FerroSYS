// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The TLS keys of a table that names a remote service, and the oldest TLS
//! version a listener or a client speaks.
//!
//! They live here, below every crate that writes one, so
//! `ferrohealth-telemetry`, `ferrohealth-server` and `ferrohealth-smart`
//! share one table. The section holds paths and an inline secret and reads
//! no file: `ferrohealth-server`'s `tls_client::TlsClient` is the one reader
//! of it. No specification governs the keys: our own design.

use std::path::PathBuf;

use ferrohealth_secret::secret::Secret;
use serde::{Deserialize, Serialize};

/// The outbound TLS keys of one table.
///
/// A table takes `client_identity`, inline, or `client_identity_file`: the
/// client certificate chain and its private key in one PEM, presented for
/// mutual TLS and held as a secret; and `trust_roots_file`, a PEM bundle of
/// trust roots beside the platform's. `Debug` and serialization show the
/// inline identity as `***`, as [`Secret`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct TlsClientSection {
    /// The client certificate chain and its private key, in one PEM.
    #[cfg_attr(feature = "schema", schemars(with = "Option<String>"))]
    pub client_identity: Option<Secret>,
    /// A file holding `client_identity`.
    pub client_identity_file: Option<PathBuf>,
    /// A PEM bundle of trust roots beside the platform's.
    pub trust_roots_file: Option<PathBuf>,
    /// The oldest TLS version the client speaks: `"1.3"` unless lowered to
    /// `"1.2"`, as for the listener.
    pub min_version: TlsVersion,
}

impl TlsClientSection {
    /// Whether the table names any TLS material: an identity or trust
    /// roots.
    #[must_use]
    pub const fn names_material(&self) -> bool {
        self.client_identity.is_some()
            || self.client_identity_file.is_some()
            || self.trust_roots_file.is_some()
    }
}

/// The oldest TLS version a listener or an outbound client speaks.
///
/// TLS 1.3 (RFC 8446) is the default, as RFC 9325 §3.1.1 has it supported; a
/// deployment may lower the minimum to TLS 1.2, which RFC 9325 §3.1.1 allows,
/// with the suites of RFC 9325 §4.2. TLS 1.0 and 1.1 are deprecated (RFC
/// 8996) and refused when the configuration is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum TlsVersion {
    /// TLS 1.2 and 1.3.
    Tls12,
    /// TLS 1.3 alone.
    #[default]
    Tls13,
}

/// A `min_version` no FerroHEALTH product speaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TlsVersionError {
    /// TLS 1.0 or 1.1, which RFC 8996 deprecates.
    #[error("TLS {0} is deprecated (RFC 8996); the minimum is `1.2` or `1.3`")]
    Deprecated(&'static str),
    /// A value that names no TLS version.
    #[error("min_version takes `1.2` or `1.3`")]
    Unknown,
}

impl TryFrom<String> for TlsVersion {
    type Error = TlsVersionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "1.3" => Ok(Self::Tls13),
            "1.2" => Ok(Self::Tls12),
            "1.1" => Err(TlsVersionError::Deprecated("1.1")),
            "1.0" => Err(TlsVersionError::Deprecated("1.0")),
            _ => Err(TlsVersionError::Unknown),
        }
    }
}

impl From<TlsVersion> for String {
    fn from(version: TlsVersion) -> Self {
        match version {
            TlsVersion::Tls12 => "1.2",
            TlsVersion::Tls13 => "1.3",
        }
        .to_owned()
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for TlsVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("TlsVersion")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ferrohealth_config::tls::TlsVersion")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": ["1.2", "1.3"],
            "description": "The oldest TLS version spoken: \"1.3\" (RFC 8446), or \"1.2\" with the RFC 9325 §4.2 suites."
        })
    }
}
