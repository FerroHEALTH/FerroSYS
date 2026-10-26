// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The listener's TLS, and the TLS the healthcheck speaks to it.
//!
//! The listener serves the certificate chain and the private key
//! `[server.tls]` names, TLS 1.3 and 1.2 (RFC 8446, RFC 5246) over rustls
//! with the aws-lc-rs provider, offering HTTP/2 and HTTP/1.1 by ALPN
//! (RFC 7301). The key file is read through `ferrohealth-secret`, so its
//! text is zeroed once the key is taken.
//!
//! The healthcheck asks over loopback, where the listener's certificate
//! names no address it connects to, so it accepts exactly the leaf
//! certificate the listener's own file holds and no other.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{
    CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use secrecy::ExposeSecret as _;

use crate::section::TlsSection;

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
}

/// The crypto provider of the listener and the healthcheck.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
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

/// Returns the listener's TLS configuration for `section`.
///
/// # Errors
///
/// Returns [`TlsError::Read`] and [`TlsError::Key`] for a file that cannot
/// be read, [`TlsError::Pem`] for one that holds no certificate or no key,
/// and [`TlsError::Config`] when the key does not match the certificate.
pub fn server_config(section: &TlsSection) -> Result<Arc<ServerConfig>, TlsError> {
    let chain = chain("server.tls.certificate_file", &section.certificate_file)?;
    let pem = ferrohealth_secret::file::read_secret_file("server.tls.key", &section.key_file)?;
    let key =
        PrivateKeyDer::from_pem_slice(pem.expose_secret().as_bytes()).map_err(|_unreadable| {
            TlsError::Pem {
                key: "server.tls.key_file",
                kind: "private key",
            }
        })?;
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(TlsError::Config)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(TlsError::Config)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The healthcheck's TLS: it accepts exactly the leaf certificate the
/// listener's certificate file holds.
#[derive(Debug, Clone)]
pub struct HealthcheckTls {
    config: Arc<ClientConfig>,
}

impl HealthcheckTls {
    /// Reads the leaf certificate the listener serves from `section`.
    ///
    /// # Errors
    ///
    /// Returns [`TlsError::Read`] and [`TlsError::Pem`] for a certificate
    /// file that cannot be read, and [`TlsError::Config`] when rustls
    /// refuses the configuration.
    pub fn read(section: &TlsSection) -> Result<Self, TlsError> {
        let leaf = chain("server.tls.certificate_file", &section.certificate_file)?
            .into_iter()
            .next()
            .ok_or(TlsError::Pem {
                key: "server.tls.certificate_file",
                kind: "certificate",
            })?;
        let provider = provider();
        let pinned = Pinned {
            leaf,
            algorithms: provider.signature_verification_algorithms,
        };
        let config = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(TlsError::Config)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(pinned))
            .with_no_client_auth();
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
