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

/// Returns the listener's TLS configuration for `section`.
///
/// # Errors
///
/// Returns [`TlsError::Read`] and [`TlsError::Key`] for a file that cannot
/// be read, [`TlsError::Pem`] for one that holds no certificate or no key,
/// [`TlsError::Config`] when the key does not match the certificate, and
/// [`TlsError::ClientAuth`] when `client_auth` and `client_ca_file` disagree.
pub fn server_config(section: &TlsSection) -> Result<Arc<ServerConfig>, TlsError> {
    server_config_of(section, TlsKeys::SERVER)
}

/// The listener's TLS configuration from `section`, every fault naming the
/// key of `keys`, as [`server_config`] reads `[server.tls]`.
///
/// # Errors
///
/// Returns every error of [`server_config`].
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
    /// Returns every error of [`server_config`].
    pub fn new(section: &TlsSection) -> Result<Self, TlsError> {
        Self::of(section, TlsKeys::SERVER)
    }

    /// Reads `section`'s files, every fault naming the key of `keys`.
    ///
    /// # Errors
    ///
    /// Returns every error of [`server_config`].
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
    /// Returns every error of [`server_config`]; the running configuration
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
    /// the identity the healthcheck presents when `section` names one.
    ///
    /// # Errors
    ///
    /// Returns [`TlsError::Read`] and [`TlsError::Pem`] for a certificate
    /// file that cannot be read, and [`TlsError::Config`] when rustls
    /// refuses the configuration.
    pub fn read(section: &TlsSection) -> Result<Self, TlsError> {
        Self::read_of(section, TlsKeys::SERVER)
    }

    /// Reads as [`HealthcheckTls::read`] does, every fault naming the key of
    /// `keys`.
    ///
    /// # Errors
    ///
    /// Returns every error of [`HealthcheckTls::read`].
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
mod tests {
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::routing::get;
    use ferrohealth_health::lifecycle::Lifecycle;
    use ferrohealth_health::registry::Registry;
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::{HealthcheckTls, PeerCertificates, ReloadableTls, TlsError, provider};
    use crate::health::Health;
    use crate::healthcheck::{Outcome, check_tls, check_url};
    use crate::section::{ClientAuth, ServerSection, TlsSection};
    use crate::stack::{StackPolicy, app};

    /// The files of one test: a server certificate, a client CA and a client
    /// identity signed by it.
    struct Files {
        dir: tempfile::TempDir,
    }

    impl Files {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a directory");
            let server = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
                .expect("a server certificate");
            write(dir.path(), "server.pem", &server.cert.pem());
            write(
                dir.path(),
                "server.key",
                &server.signing_key.serialize_pem(),
            );
            let ca_key = KeyPair::generate().expect("a CA key");
            let mut ca_params =
                CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
            write(dir.path(), "ca.pem", &ca.pem());
            let issuer = Issuer::new(ca_params, ca_key);
            let client_key = KeyPair::generate().expect("a client key");
            let client = CertificateParams::new(vec!["client.example".to_owned()])
                .expect("client parameters")
                .signed_by(&client_key, &issuer)
                .expect("a client certificate");
            write(
                dir.path(),
                "client.pem",
                &format!("{}{}", client.pem(), client_key.serialize_pem()),
            );
            Self { dir }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.dir.path().join(name)
        }

        fn section(&self, client_auth: ClientAuth, identity: bool) -> TlsSection {
            TlsSection {
                client_auth,
                client_ca_file: (client_auth != ClientAuth::None).then(|| self.path("ca.pem")),
                healthcheck_identity_file: identity.then(|| self.path("client.pem")),
                ..TlsSection::new(self.path("server.pem"), self.path("server.key"))
            }
        }
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).expect("the file is written");
    }

    /// Serves a `/peer` route that answers how many client certificates it
    /// saw, over `section`'s TLS, until the test ends.
    async fn start(section: &TlsSection) -> SocketAddr {
        let tls = Arc::new(ReloadableTls::new(section).expect("the files read"));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let lifecycle = Lifecycle::default();
        let product = Router::new().route(
            "/peer",
            get(
                |peers: Option<axum::Extension<PeerCertificates>>,
                 client: Option<axum::Extension<crate::client_address::ClientAddress>>| async move {
                    let seen = peers.map_or(0, |axum::Extension(peers)| peers.0.len());
                    match client {
                        Some(axum::Extension(client)) if client.0.is_loopback() => seen.to_string(),
                        _ => "the connection's address did not reach the handler".to_owned(),
                    }
                },
            ),
        );
        let app = app(
            product,
            Health::new(Registry::default(), lifecycle.clone()),
            &ServerSection::default(),
            StackPolicy::default(),
        );
        tokio::spawn(crate::serve::serve_tls(
            listener,
            app,
            tls,
            lifecycle,
            std::future::pending::<()>(),
            Duration::ZERO,
            Duration::from_secs(1),
        ));
        address
    }

    /// The body of `GET /peer` over `tls`, or `None` when no answer came: how
    /// many client certificates the handler saw, which it answers only when
    /// the connection's client address reached it too.
    async fn peer(address: SocketAddr, tls: &HealthcheckTls) -> Option<String> {
        let stream = tokio::net::TcpStream::connect(address).await.ok()?;
        let name = rustls::pki_types::ServerName::IpAddress(address.ip().into());
        let mut stream = tokio_rustls::TlsConnector::from(tls.config())
            .connect(name, stream)
            .await
            .ok()?;
        stream
            .write_all(b"GET /peer HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
            .await
            .ok()?;
        let mut answer = Vec::new();
        let _read = stream.read_to_end(&mut answer).await;
        let answer = String::from_utf8(answer).ok()?;
        answer
            .starts_with("HTTP/1.1 200")
            .then(|| answer.rsplit("\r\n").next().unwrap_or_default().to_owned())
    }

    #[tokio::test]
    async fn a_required_client_certificate_is_verified_and_reaches_the_handler() {
        let files = Files::new();
        let address = start(&files.section(ClientAuth::Required, false)).await;
        let with = HealthcheckTls::read(&files.section(ClientAuth::Required, true))
            .expect("the identity reads");
        let without = HealthcheckTls::read(&files.section(ClientAuth::Required, false))
            .expect("the pin reads");
        assert_eq!(
            Some("1".to_owned()),
            peer(address, &with).await,
            "the chain reaches the handler"
        );
        assert_eq!(
            None,
            peer(address, &without).await,
            "no certificate, no answer"
        );
        assert_eq!(
            Outcome::Healthy,
            check_tls(address, "", Duration::from_secs(5), &with).await
        );
    }

    #[tokio::test]
    async fn an_optional_client_certificate_is_taken_when_presented() {
        let files = Files::new();
        let address = start(&files.section(ClientAuth::Optional, false)).await;
        let with = HealthcheckTls::read(&files.section(ClientAuth::Optional, true))
            .expect("the identity reads");
        let without = HealthcheckTls::read(&files.section(ClientAuth::Optional, false))
            .expect("the pin reads");
        assert_eq!(Some("1".to_owned()), peer(address, &with).await);
        assert_eq!(Some("0".to_owned()), peer(address, &without).await);
    }

    #[tokio::test]
    async fn without_client_auth_no_certificate_is_asked() {
        let files = Files::new();
        let address = start(&files.section(ClientAuth::None, false)).await;
        let pin =
            HealthcheckTls::read(&files.section(ClientAuth::None, false)).expect("the pin reads");
        assert_eq!(Some("0".to_owned()), peer(address, &pin).await);
        let url = format!("https://127.0.0.1:{}/health/readiness", address.port());
        assert_eq!(
            Outcome::Healthy,
            check_url(&url, Duration::from_secs(5), Some(&pin)).await
        );
        assert!(
            matches!(
                check_url(&url, Duration::from_secs(5), None).await,
                Outcome::Failed(_)
            ),
            "https needs the pin"
        );
    }

    #[test]
    fn the_minimum_version_decides_whether_tls_1_2_is_spoken() {
        let versions = |minimum| -> Vec<rustls::ProtocolVersion> {
            super::protocol_versions(minimum)
                .iter()
                .map(|supported| supported.version)
                .collect()
        };
        assert_eq!(
            vec![
                rustls::ProtocolVersion::TLSv1_3,
                rustls::ProtocolVersion::TLSv1_2
            ],
            versions(crate::section::TlsVersion::Tls12)
        );
        assert_eq!(
            vec![rustls::ProtocolVersion::TLSv1_3],
            versions(crate::section::TlsVersion::Tls13)
        );
        let files = Files::new();
        let strict = TlsSection {
            min_version: crate::section::TlsVersion::Tls13,
            ..files.section(ClientAuth::None, false)
        };
        assert!(
            super::server_config(&strict).is_ok(),
            "a 1.3-only listener builds"
        );
    }

    #[test]
    fn a_reload_installs_valid_files_and_keeps_the_running_ones_otherwise() {
        let files = Files::new();
        let tls =
            ReloadableTls::new(&files.section(ClientAuth::None, false)).expect("the files read");
        let before = tls.current();
        let fresh = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("a certificate");
        write(files.dir.path(), "server.pem", &fresh.cert.pem());
        write(
            files.dir.path(),
            "server.key",
            &fresh.signing_key.serialize_pem(),
        );
        tls.reload().expect("valid files reload");
        assert!(
            !Arc::ptr_eq(&before, &tls.current()),
            "the next handshake takes the new files"
        );
        let installed = tls.current();
        write(files.dir.path(), "server.key", "no key here");
        assert!(
            matches!(tls.reload(), Err(TlsError::Pem { .. })),
            "a key that does not read is refused"
        );
        assert!(
            Arc::ptr_eq(&installed, &tls.current()),
            "the running configuration stays"
        );
    }

    #[test]
    fn tls12_offers_only_the_rfc_9325_suites() {
        use rustls::CipherSuite;
        let suites: Vec<CipherSuite> = provider()
            .cipher_suites
            .iter()
            .filter(|suite| suite.tls13().is_none())
            .map(rustls::SupportedCipherSuite::suite)
            .collect();
        assert_eq!(
            vec![
                CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
                CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
                CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
                CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
            ],
            suites
        );
        assert!(
            provider()
                .cipher_suites
                .iter()
                .any(|suite| suite.tls13().is_some()),
            "TLS 1.3 stays"
        );
    }

    #[test]
    fn client_auth_without_a_ca_is_refused_at_load() {
        let error = toml::from_str::<TlsSection>(
            "certificate_file = \"a\"\nkey_file = \"b\"\nclient_auth = \"required\"\n",
        )
        .expect_err("a required client certificate needs a CA");
        assert!(error.message().contains("client_ca_file"), "{error}");
        let section: TlsSection = toml::from_str("certificate_file = \"a\"\nkey_file = \"b\"\n")
            .expect("the plain table loads");
        assert_eq!(TlsSection::new("a".into(), "b".into()), section);
    }

    #[test]
    fn a_ca_under_no_client_auth_is_refused_at_load() {
        for table in [
            "certificate_file = \"a\"\nkey_file = \"b\"\nclient_ca_file = \"ca.pem\"\n",
            "certificate_file = \"a\"\nkey_file = \"b\"\nclient_auth = \"none\"\nclient_ca_file = \"ca.pem\"\n",
        ] {
            let error = toml::from_str::<TlsSection>(table)
                .expect_err("a CA under client_auth none would ask no certificate");
            let message = error.message();
            assert!(message.contains("client_ca_file"), "{error}");
            assert!(
                message.contains("required") && message.contains("optional"),
                "the refusal says what to set: {error}"
            );
        }
    }

    #[test]
    fn a_section_built_in_code_with_a_ca_under_no_client_auth_is_refused() {
        let files = Files::new();
        let section = TlsSection {
            client_ca_file: Some(files.path("ca.pem")),
            ..files.section(ClientAuth::None, false)
        };
        assert!(matches!(
            super::server_config(&section),
            Err(TlsError::ClientAuth(
                crate::section::ClientAuthError::UnusedClientCa
            ))
        ));
        let section = TlsSection {
            client_ca_file: None,
            ..files.section(ClientAuth::Required, false)
        };
        assert!(matches!(
            super::server_config(&section),
            Err(TlsError::ClientAuth(
                crate::section::ClientAuthError::NoClientCa(_)
            ))
        ));
    }
}
