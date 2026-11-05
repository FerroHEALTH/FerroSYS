// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The listener's TLS, and the TLS the healthcheck speaks to it.
//!
//! The listener serves the certificate chain and the private key
//! `[server.tls]` names, TLS 1.3 and 1.2 (RFC 8446, RFC 5246) over rustls
//! with the aws-lc-rs provider, offering HTTP/2 and HTTP/1.1 by ALPN
//! (RFC 7301). It prefers TLS 1.3 and offers only the four forward-secret
//! TLS 1.2 cipher suites RFC 9325 §4.2 recommends (BCP 195). It asks a
//! client certificate as `client_auth` says, verified against
//! `client_ca_file`, and hands a verified chain to the handlers as
//! [`PeerCertificates`]. The key file is read through `ferrohealth-secret`,
//! so its text is zeroed once the key is taken. [`ReloadableTls`] reads the
//! files again on demand: a reload that does not read keeps the running
//! configuration, and an open connection keeps the one it began with.
//!
//! The healthcheck asks over loopback, where the listener's certificate
//! names no address it connects to, so it accepts exactly the leaf
//! certificate the listener's own file holds and no other.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use secrecy::ExposeSecret as _;

use crate::section::{ClientAuth, NoClientCa, TlsSection};

/// The listener's TLS could not be set up.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TlsError {
    /// A file could not be read.
    #[error("{key} names {}, which could not be read", path.display())]
    Read {
        /// The key that names the file.
        key: &'static str,
        /// The file.
        path: PathBuf,
        /// What the file system reported.
        #[source]
        source: std::io::Error,
    },
    /// The key file could not be read.
    #[error(transparent)]
    Key(#[from] ferrohealth_secret::file::SecretError),
    /// A file holds no PEM object of the kind its key names.
    #[error("{key} holds no {kind} in PEM")]
    Pem {
        /// The key that names the file.
        key: &'static str,
        /// What the file should hold.
        kind: &'static str,
    },
    /// rustls refused the chain, the key, or the pair.
    #[error("the TLS configuration was refused")]
    Config(#[source] rustls::Error),
    /// `client_auth` and `client_ca_file` disagree: a CA under `none`, which
    /// would ask no client certificate, or a client certificate asked with no
    /// CA to verify it against.
    #[error(transparent)]
    ClientAuth(#[from] crate::section::ClientAuthError),
    /// The client CA bundle of `[server.tls]` could not serve as trust
    /// anchors; no longer returned, [`TlsError::ClientCaRefused`] names the
    /// listener's own key.
    #[deprecated(note = "TlsError::ClientCaRefused names the listener's own key")]
    #[error("server.tls.client_ca_file holds no CA a client certificate can chain to")]
    ClientCa(#[source] rustls::server::VerifierBuilderError),
    /// The client CA bundle could not serve as trust anchors.
    #[error("{key} holds no CA a client certificate can chain to")]
    ClientCaRefused {
        /// The key that names the bundle.
        key: &'static str,
        /// What rustls reported.
        #[source]
        source: rustls::server::VerifierBuilderError,
    },
}

/// The configuration keys a listener's TLS table is read from, which every
/// [`TlsError`] names.
///
/// [`TlsKeys::SERVER`] is `[server.tls]`; a second listener of
/// `serve::run_all` configured from another table names its own with
/// [`crate::tls_keys!`], such as `tls_keys!("metrics.tls")`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsKeys {
    certificate: &'static str,
    key: &'static str,
    client_ca: &'static str,
    healthcheck_identity: &'static str,
}

impl TlsKeys {
    /// The keys of `[server.tls]`.
    pub const SERVER: Self = crate::tls_keys!("server.tls");

    /// The keys `certificate_file`, `key_file`, `client_ca_file` and
    /// `healthcheck_identity_file` of one table, each spelled in full.
    #[must_use]
    pub const fn new(
        certificate: &'static str,
        key: &'static str,
        client_ca: &'static str,
        healthcheck_identity: &'static str,
    ) -> Self {
        Self {
            certificate,
            key,
            client_ca,
            healthcheck_identity,
        }
    }

    /// The key of the certificate chain.
    #[must_use]
    pub const fn certificate_file(&self) -> &'static str {
        self.certificate
    }

    /// The key of the private key.
    #[must_use]
    pub const fn key_file(&self) -> &'static str {
        self.key
    }

    /// The key of the client CA bundle.
    #[must_use]
    pub const fn client_ca_file(&self) -> &'static str {
        self.client_ca
    }

    /// The key of the healthcheck's identity.
    #[must_use]
    pub const fn healthcheck_identity_file(&self) -> &'static str {
        self.healthcheck_identity
    }
}

/// The [`TlsKeys`] of the table `$table`, such as `"metrics.tls"`.
#[macro_export]
macro_rules! tls_keys {
    ($table:literal) => {
        $crate::tls::TlsKeys::new(
            concat!($table, ".certificate_file"),
            concat!($table, ".key_file"),
            concat!($table, ".client_ca_file"),
            concat!($table, ".healthcheck_identity_file"),
        )
    };
}

/// The crypto provider of the listener and the healthcheck.
pub(crate) fn provider() -> Arc<CryptoProvider> {
    use rustls::crypto::aws_lc_rs::{cipher_suite, default_provider};
    let mut provider = default_provider();
    // NOTE: RFC 9325 §4.2 recommends exactly these four TLS 1.2 suites, every one
    // forward secret (§7.3); the TLS 1.3 suites stay the provider's.
    provider
        .cipher_suites
        .retain(|suite| suite.tls13().is_some());
    provider.cipher_suites.extend([
        cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        cipher_suite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        cipher_suite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    ]);
    Arc::new(provider)
}

/// Reads the private key in the PEM file `path`, the value of `key`.
fn private_key(key: &'static str, path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let pem =
        ferrohealth_secret::file::read_secret_file(key.strip_suffix("_file").unwrap_or(key), path)?;
    PrivateKeyDer::from_pem_slice(pem.expose_secret().as_bytes()).map_err(|_unreadable| {
        TlsError::Pem {
            key,
            kind: "private key",
        }
    })
}

/// Reads the certificate chain at `path`, the value of `key`.
fn chain(key: &'static str, path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let text = std::fs::read(path).map_err(|source| TlsError::Read {
        key,
        path: path.to_path_buf(),
        source,
    })?;
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&text)
        .collect::<Result<_, _>>()
        .map_err(|_unreadable| TlsError::Pem {
            key,
            kind: "certificate",
        })?;
    if chain.is_empty() {
        return Err(TlsError::Pem {
            key,
            kind: "certificate",
        });
    }
    Ok(chain)
}

/// The versions the listener speaks from `min_version` on.
pub(crate) fn protocol_versions(
    min_version: crate::section::TlsVersion,
) -> &'static [&'static rustls::SupportedProtocolVersion] {
    static TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
    match min_version {
        crate::section::TlsVersion::Tls12 => rustls::ALL_VERSIONS,
        crate::section::TlsVersion::Tls13 => TLS13_ONLY,
    }
}

/// The listener's TLS configuration from `section`, every fault naming the
/// key of `keys`; `[server.tls]` is read with [`TlsKeys::SERVER`].
///
/// # Errors
///
/// Returns [`TlsError::Read`] and [`TlsError::Key`] for a file that cannot
/// be read, [`TlsError::Pem`] for one that holds no certificate or no key,
/// [`TlsError::Config`] when the key does not match the certificate, and
/// [`TlsError::ClientAuth`] when `client_auth` and `client_ca_file` disagree.
pub fn server_config_of(
    section: &TlsSection,
    keys: TlsKeys,
) -> Result<Arc<ServerConfig>, TlsError> {
    let served = chain(keys.certificate, &section.certificate_file)?;
    let key = private_key(keys.key, &section.key_file)?;
    let provider = provider();
    let versions = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(protocol_versions(section.min_version))
        .map_err(TlsError::Config)?;
    let builder = match (section.client_auth, &section.client_ca_file) {
        (ClientAuth::None, None) => versions.with_no_client_auth(),
        (ClientAuth::None, Some(_)) => {
            return Err(crate::section::ClientAuthError::UnusedClientCa.into());
        }
        (_, None) => return Err(crate::section::ClientAuthError::from(NoClientCa).into()),
        (policy, Some(path)) => {
            let mut roots = RootCertStore::empty();
            for certificate in chain(keys.client_ca, path)? {
                roots.add(certificate).map_err(TlsError::Config)?;
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider);
            let verifier = if policy == ClientAuth::Optional {
                verifier.allow_unauthenticated()
            } else {
                verifier
            };
            versions.with_client_cert_verifier(verifier.build().map_err(|source| {
                TlsError::ClientCaRefused {
                    key: keys.client_ca,
                    source,
                }
            })?)
        }
    };
    let mut config = builder
        .with_single_cert(served, key)
        .map_err(TlsError::Config)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The verified client certificate chain of a TLS connection, the leaf
/// first, which the listener puts in every request's extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCertificates(pub Arc<[CertificateDer<'static>]>);

/// The listener's TLS configuration, read again on demand.
///
/// `serve::run` builds one from `[server.tls]` and reads it again on
/// `SIGHUP` and every `reload_interval_s`. A product that reloads its
/// configuration on a path of its own builds it itself, hands it to
/// `serve::run_with_tls` or `serve::Listener::with_reloadable_tls`, and calls
/// [`ReloadableTls::reload`] from that path.
#[derive(Debug)]
pub struct ReloadableTls {
    section: TlsSection,
    keys: TlsKeys,
    current: RwLock<Arc<ServerConfig>>,
}

impl ReloadableTls {
    /// Reads `section`'s files.
    ///
    /// # Errors
    ///
    /// Returns every error of [`server_config_of`].
    pub fn new(section: &TlsSection) -> Result<Self, TlsError> {
        Self::of(section, TlsKeys::SERVER)
    }

    /// Reads `section`'s files, every fault naming the key of `keys`.
    ///
    /// # Errors
    ///
    /// Returns every error of [`server_config_of`].
    pub fn of(section: &TlsSection, keys: TlsKeys) -> Result<Self, TlsError> {
        Ok(Self {
            current: RwLock::new(server_config_of(section, keys)?),
            section: section.clone(),
            keys,
        })
    }

    /// The keys every fault names.
    #[must_use]
    pub const fn keys(&self) -> TlsKeys {
        self.keys
    }

    /// The table the files come from.
    #[must_use]
    pub const fn section(&self) -> &TlsSection {
        &self.section
    }

    /// The configuration the next handshake uses.
    #[must_use]
    pub fn current(&self) -> Arc<ServerConfig> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Reads the files again and installs them for the next handshake.
    ///
    /// A product calls it from its own reload path too, and refuses its
    /// change on an error.
    ///
    /// # Errors
    ///
    /// Returns every error of [`server_config_of`]; the running configuration
    /// stays.
    pub fn reload(&self) -> Result<(), TlsError> {
        let fresh = server_config_of(&self.section, self.keys)?;
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = fresh;
        Ok(())
    }
}

/// The healthcheck's TLS: it accepts exactly the leaf certificate the
/// listener's certificate file holds.
#[derive(Debug, Clone)]
pub struct HealthcheckTls {
    config: Arc<ClientConfig>,
}

impl HealthcheckTls {
    /// Reads the leaf certificate the listener serves from `section`, and
    /// the identity the healthcheck presents when `section` names one,
    /// every fault naming the key of `keys`; `[server.tls]` is read with
    /// [`TlsKeys::SERVER`].
    ///
    /// # Errors
    ///
    /// Returns [`TlsError::Read`] and [`TlsError::Pem`] for a certificate
    /// file that cannot be read, and [`TlsError::Config`] when rustls
    /// refuses the configuration.
    pub fn read_of(section: &TlsSection, keys: TlsKeys) -> Result<Self, TlsError> {
        let leaf = chain(keys.certificate, &section.certificate_file)?
            .into_iter()
            .next()
            .ok_or(TlsError::Pem {
                key: keys.certificate,
                kind: "certificate",
            })?;
        let provider = provider();
        let pinned = Pinned {
            leaf,
            algorithms: provider.signature_verification_algorithms,
        };
        let builder = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(TlsError::Config)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(pinned));
        let config = match &section.healthcheck_identity_file {
            None => builder.with_no_client_auth(),
            Some(path) => {
                let key = keys.healthcheck_identity;
                builder
                    .with_client_auth_cert(chain(key, path)?, private_key(key, path)?)
                    .map_err(TlsError::Config)?
            }
        };
        Ok(Self {
            config: Arc::new(config),
        })
    }

    /// The client configuration.
    pub(crate) fn config(&self) -> Arc<ClientConfig> {
        Arc::clone(&self.config)
    }
}

/// The verifier that accepts one certificate: the listener's own.
#[derive(Debug)]
struct Pinned {
    leaf: CertificateDer<'static>,
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // NOTE: no specification governs this: our own design; over loopback the
        // certificate names no address asked, so the one file it came from is the trust.
        if end_entity.as_ref() == self.leaf.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests;
