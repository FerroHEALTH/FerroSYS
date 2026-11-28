// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The TLS material of a product's outbound TLS calls, HTTPS and others.
//!
//! A table that names a remote service holds a
//! `ferrohealth_config::tls::TlsClientSection`: `client_identity`, inline, or
//! `client_identity_file`: the client certificate chain and its private key
//! in one PEM, presented for mutual TLS and held as a secret;
//! `trust_roots_file`, a PEM bundle of trust roots beside the platform's,
//! whose chains are checked for revocation; and
//! `trust_roots_without_revocation_file`, roots whose PKI publishes no
//! revocation service. [`TlsClient::read`] reads every file once, and
//! [`TlsClient::certificate`] returns the identity's end-entity certificate;
//! [`TlsClient::client_config`] gives a rustls configuration over the
//! platform roots the caller hands it, and `TlsClient::platform_config`
//! (feature `platform-verifier`) one that verifies a chain to the table's
//! roots with webpki and any other chain with the platform's verifier. No
//! specification governs the keys: our own design.

use std::path::PathBuf;

#[cfg(feature = "platform-verifier")]
mod platform;
#[cfg(feature = "platform-verifier")]
pub mod platform_tls;

use ferrohealth_config::tls::TlsClientSection;
use ferrohealth_secret::secret::Secret;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};

/// The outbound TLS material could not be read.
///
/// No variant carries any part of a certificate or a key.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TlsClientError {
    /// The identity is set twice, or its file could not be read.
    #[error(transparent)]
    Identity(#[from] ferrohealth_secret::file::SecretError),
    /// The trust roots file could not be read.
    #[error("{key} names {}, which could not be read", path.display())]
    Read {
        /// The key that names the file.
        key: String,
        /// The file.
        path: PathBuf,
        /// What the file system reported.
        #[source]
        source: std::io::Error,
    },
    /// The material holds no PEM of the kind its key takes.
    #[error("{key} holds no {holds} in PEM")]
    Pem {
        /// The key the material was read from.
        key: String,
        /// What it should hold.
        holds: &'static str,
    },
    /// rustls refused the identity or a root.
    #[error("{key} was refused by the TLS library")]
    Refused {
        /// The key the material was read from.
        key: String,
        /// What rustls reported.
        #[source]
        source: rustls::Error,
    },
    /// A root's subject does not decode, so it cannot be named.
    #[cfg(feature = "platform-verifier")]
    #[error("{key} holds a root whose subject does not decode")]
    Subject {
        /// The key the roots were read from.
        key: String,
        /// What the decoder reported.
        #[source]
        source: der::Error,
    },
    /// rustls refused to build a verifier over the roots.
    #[cfg(feature = "platform-verifier")]
    #[error("{key} gives no verifier")]
    Verifier {
        /// The key the roots were read from.
        key: String,
        /// What rustls reported.
        #[source]
        source: rustls::client::VerifierBuilderError,
    },
    /// The revocation check of the table's roots cannot be built.
    #[cfg(feature = "platform-verifier")]
    #[error("{key} cannot be checked for revocation")]
    Revocation {
        /// The key the roots were read from.
        key: String,
        /// What the check reported.
        #[source]
        source: crate::revocation::RevocationSetupError,
    },
}

/// The client identity: its certificate chain and private key.
struct Identity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

/// The outbound TLS material of one table, every file read.
///
/// Two reads are equal when they read the same material: the identity's
/// chain and private key, the extra roots and the minimum version. The table
/// key takes no part, so a reload compares what the files held, and a
/// certificate renewed at the same path compares unequal.
pub struct TlsClient {
    key: String,
    identity: Option<Identity>,
    roots: Vec<CertificateDer<'static>>,
    unchecked_roots: Vec<CertificateDer<'static>>,
    min_version: ferrohealth_config::tls::TlsVersion,
}

impl std::fmt::Debug for TlsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClient")
            .field("key", &self.key)
            .field("identity", &self.identity.as_ref().map(|_identity| "***"))
            .field("roots", &self.roots.len())
            .field("unchecked_roots", &self.unchecked_roots.len())
            .field("min_version", &self.min_version)
            .finish()
    }
}

impl PartialEq for TlsClient {
    fn eq(&self, other: &Self) -> bool {
        let identity = match (&self.identity, &other.identity) {
            (None, None) => true,
            (Some(ours), Some(theirs)) => {
                ours.chain == theirs.chain && ours.key.secret_der() == theirs.key.secret_der()
            }
            _ => false,
        };
        identity
            && self.roots == other.roots
            && self.unchecked_roots == other.unchecked_roots
            && self.min_version == other.min_version
    }
}

impl Eq for TlsClient {}

impl TlsClient {
    /// Reads the material `section` names, the table at `key` (such as
    /// `xcpd`), so an error names `xcpd.client_identity`.
    ///
    /// # Errors
    ///
    /// Returns [`TlsClientError::Identity`] for an identity set inline and
    /// as a file or a file that cannot be read, [`TlsClientError::Read`] for
    /// a roots file that cannot be read, and [`TlsClientError::Pem`] for an
    /// identity without a certificate and a private key or a roots file
    /// without a certificate.
    pub fn read(key: &str, section: &TlsClientSection) -> Result<Self, TlsClientError> {
        let identity_key = format!("{key}.client_identity");
        let identity = ferrohealth_secret::file::read_secret(
            &identity_key,
            section.client_identity.clone(),
            section.client_identity_file.as_deref(),
        )?
        .map(|pem: Secret| {
            let text = pem.expose().as_bytes();
            let pem_error = || TlsClientError::Pem {
                key: identity_key.clone(),
                holds: "certificate chain and private key",
            };
            let chain = certificates(text).ok_or_else(pem_error)?;
            let key = PrivateKeyDer::from_pem_slice(text).map_err(|_unreadable| pem_error())?;
            Ok::<_, TlsClientError>(Identity { chain, key })
        })
        .transpose()?;
        Ok(Self {
            key: key.to_owned(),
            identity,
            roots: roots(key, "trust_roots_file", section.trust_roots_file.as_ref())?,
            unchecked_roots: roots(
                key,
                "trust_roots_without_revocation_file",
                section.trust_roots_without_revocation_file.as_ref(),
            )?,
            min_version: section.min_version,
        })
    }

    /// Whether a client identity is presented.
    #[must_use]
    pub const fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// The end-entity certificate of the client identity, the first
    /// certificate of its chain (RFC 8446 §4.4.2), or `None` without an
    /// identity.
    ///
    /// It is the certificate the handshake sends, public material, so a
    /// product takes what it binds to the identity from it, such as the
    /// certificate thumbprint of RFC 8705 §3.1, without reading the file a
    /// second time.
    #[must_use]
    pub fn certificate(&self) -> Option<&CertificateDer<'static>> {
        self.identity
            .as_ref()
            .and_then(|identity| identity.chain.first())
    }

    /// The extra roots, the identity's chain and key, and the minimum
    /// version, for a crate module that builds its own TLS over them.
    #[cfg(feature = "revocation")]
    pub(crate) fn parts(&self) -> crate::revocation::Parts<'_> {
        crate::revocation::Parts {
            roots: &self.roots,
            identity: self
                .identity
                .as_ref()
                .map(|identity| (identity.chain.as_slice(), &identity.key)),
            min_version: self.min_version,
        }
    }

    /// The rustls configuration that trusts `platform_roots` and both sets
    /// of extra roots, and presents the client identity when one is set.
    ///
    /// The protocol versions and cipher suites are the listener's: TLS 1.3,
    /// and TLS 1.2 with the four suites RFC 9325 §4.2 recommends only where
    /// `min_version` lowers the minimum.
    ///
    /// # Errors
    ///
    /// Returns [`TlsClientError::Refused`] when rustls refuses a root or the
    /// identity.
    pub fn client_config(
        &self,
        mut platform_roots: RootCertStore,
    ) -> Result<ClientConfig, TlsClientError> {
        for root in self.roots.iter().chain(&self.unchecked_roots) {
            platform_roots
                .add(root.clone())
                .map_err(|source| self.refused("trust_roots_file", source))?;
        }
        let builder = self
            .versions(crate::tls::provider())?
            .with_root_certificates(platform_roots);
        self.authenticate(builder)
    }

    /// The platform configuration: a chain to a root of `trust_roots_file`
    /// is verified by webpki and opens a connection only once a revocation
    /// check cleared it, a chain to a root of
    /// `trust_roots_without_revocation_file` is verified by webpki alone and
    /// logged at each connection, and any other chain goes to the platform's
    /// verifier under the operating system's policy.
    ///
    /// webpki verifies over the provider's algorithms, so a chain an ML-DSA
    /// CA signs (RFC 9881) verifies on every operating system; the platform
    /// verifiers of macOS and Windows verify none. The protocol versions and
    /// cipher suites are those of [`TlsClient::client_config`]. A product
    /// clears the endpoints that reach the checked roots with
    /// [`crate::revocation::Clearing`] over [`platform_tls::PlatformTls::check`].
    ///
    /// # Errors
    ///
    /// Returns [`TlsClientError::Revocation`] when the check of the roots
    /// cannot be built and [`TlsClientError::Refused`] when rustls refuses a
    /// root, the identity, or the platform verifier cannot be built.
    #[cfg(feature = "platform-verifier")]
    pub fn platform_config(&self) -> Result<platform_tls::PlatformTls, TlsClientError> {
        let provider = crate::tls::provider();
        let check = (!self.roots.is_empty())
            .then(|| crate::revocation::RevocationCheck::from_tls_client(self))
            .transpose()
            .map_err(|source| TlsClientError::Revocation {
                key: format!("{}.trust_roots_file", self.key),
                source,
            })?;
        let unchecked = self.unchecked(&provider)?;
        let names = unchecked.as_ref().map(|unchecked| unchecked.names.clone());
        if let Some(names) = &names {
            tracing::warn!(
                key = %self.key,
                roots = %names,
                "these trust roots are trusted with no revocation check (trust_roots_without_revocation_file)"
            );
        }
        let verifier = platform::Split::new(
            check
                .as_ref()
                .map(crate::revocation::RevocationCheck::verifier),
            unchecked,
            std::sync::Arc::clone(&provider),
        )
        .map_err(|source| self.refused("trust_roots_file", source))?;
        let builder = self
            .versions(provider)?
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(verifier));
        Ok(platform_tls::PlatformTls::new(
            self.authenticate(builder)?,
            check,
            names.map(|names| (self.key.clone(), names)),
        ))
    }

    /// The webpki verifier over the roots of
    /// `trust_roots_without_revocation_file`, with their subjects, or `None`
    /// when the table names none.
    #[cfg(feature = "platform-verifier")]
    fn unchecked(
        &self,
        provider: &std::sync::Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<Option<platform::Unchecked>, TlsClientError> {
        if self.unchecked_roots.is_empty() {
            return Ok(None);
        }
        let mut store = RootCertStore::empty();
        let mut names = Vec::with_capacity(self.unchecked_roots.len());
        for root in &self.unchecked_roots {
            store
                .add(root.clone())
                .map_err(|source| self.refused("trust_roots_without_revocation_file", source))?;
            names.push(subject(root).map_err(|source| TlsClientError::Subject {
                key: format!("{}.trust_roots_without_revocation_file", self.key),
                source,
            })?);
        }
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            std::sync::Arc::new(store),
            std::sync::Arc::clone(provider),
        )
        .build()
        .map_err(|source| TlsClientError::Verifier {
            key: format!("{}.trust_roots_without_revocation_file", self.key),
            source,
        })?;
        Ok(Some(platform::Unchecked {
            webpki,
            key: self.key.clone(),
            names: names.join("; "),
        }))
    }

    /// The configuration builder over `provider` at the table's versions.
    fn versions(
        &self,
        provider: std::sync::Arc<rustls::crypto::CryptoProvider>,
    ) -> Result<rustls::ConfigBuilder<ClientConfig, rustls::WantsVerifier>, TlsClientError> {
        ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(crate::tls::protocol_versions(self.min_version))
            .map_err(|source| self.refused("client_identity", source))
    }

    /// `builder` finished with the client identity, or with none.
    fn authenticate(
        &self,
        builder: rustls::ConfigBuilder<ClientConfig, rustls::client::WantsClientCert>,
    ) -> Result<ClientConfig, TlsClientError> {
        match &self.identity {
            None => Ok(builder.with_no_client_auth()),
            Some(identity) => builder
                .with_client_auth_cert(identity.chain.clone(), identity.key.clone_key())
                .map_err(|source| self.refused("client_identity", source)),
        }
    }

    fn refused(&self, name: &str, source: rustls::Error) -> TlsClientError {
        TlsClientError::Refused {
            key: format!("{}.{name}", self.key),
            source,
        }
    }
}

/// The roots the file at `name` of table `key` holds, or none when the
/// table does not name one.
fn roots(
    key: &str,
    name: &str,
    path: Option<&PathBuf>,
) -> Result<Vec<CertificateDer<'static>>, TlsClientError> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let roots_key = format!("{key}.{name}");
    let text = std::fs::read(path).map_err(|source| TlsClientError::Read {
        key: roots_key.clone(),
        path: path.clone(),
        source,
    })?;
    certificates(&text).ok_or(TlsClientError::Pem {
        key: roots_key,
        holds: "certificate",
    })
}

/// The subject of `root` as RFC 4514 renders it.
///
/// # Errors
///
/// The decoding error when `root` is no X.509 certificate.
#[cfg(feature = "platform-verifier")]
fn subject(root: &CertificateDer<'_>) -> Result<String, der::Error> {
    use der::Decode as _;
    x509_cert::Certificate::from_der(root)
        .map(|certificate| certificate.tbs_certificate().subject().to_string())
}

/// The certificates in `pem`, or `None` when it holds none or one that does
/// not read.
fn certificates(pem: &[u8]) -> Option<Vec<CertificateDer<'static>>> {
    // NOTE: no specification governs this: our own design; a PEM object that
    // does not read makes the whole file defective, reported by the caller.
    let found: Vec<_> = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<_, _>>()
        .ok()?;
    (!found.is_empty()).then_some(found)
}

#[cfg(test)]
mod tests;
