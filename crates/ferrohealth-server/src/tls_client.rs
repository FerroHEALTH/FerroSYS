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
mod tests {
    use std::net::SocketAddr;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::routing::get;
    use ferrohealth_health::lifecycle::Lifecycle;
    use ferrohealth_secret::secret::Secret;
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use rustls::RootCertStore;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use ferrohealth_config::tls::TlsClientSection;

    use super::{TlsClient, TlsClientError};
    use crate::section::{ClientAuth, TlsSection};
    use crate::tls::{PeerCertificates, ReloadableTls};

    /// A CA, a server certificate for `localhost` and a client identity,
    /// both signed by it.
    struct Pki {
        dir: tempfile::TempDir,
    }

    impl Pki {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a directory");
            let ca_key = KeyPair::generate().expect("a CA key");
            let mut ca_params =
                CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
            write(dir.path(), "ca.pem", &ca.pem());
            let issuer = Issuer::new(ca_params, ca_key);
            for (name, san) in [("server", "localhost"), ("client", "client.example")] {
                let key = KeyPair::generate().expect("a key");
                let certificate = CertificateParams::new(vec![san.to_owned()])
                    .expect("parameters")
                    .signed_by(&key, &issuer)
                    .expect("a certificate");
                write(dir.path(), &format!("{name}.pem"), &certificate.pem());
                write(dir.path(), &format!("{name}.key"), &key.serialize_pem());
            }
            let identity = format!(
                "{}{}",
                std::fs::read_to_string(dir.path().join("client.pem")).expect("the chain"),
                std::fs::read_to_string(dir.path().join("client.key")).expect("the key")
            );
            write(dir.path(), "identity.pem", &identity);
            Self { dir }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.dir.path().join(name)
        }

        fn client(&self, identity: bool) -> TlsClientSection {
            TlsClientSection {
                client_identity_file: identity.then(|| self.path("identity.pem")),
                trust_roots_file: Some(self.path("ca.pem")),
                ..TlsClientSection::default()
            }
        }

        /// The table with the CA among the roots trusted with no revocation
        /// check, since this CA publishes no revocation service.
        #[cfg(feature = "platform-verifier")]
        fn unchecked(&self, identity: bool) -> TlsClientSection {
            TlsClientSection {
                client_identity_file: identity.then(|| self.path("identity.pem")),
                trust_roots_without_revocation_file: Some(self.path("ca.pem")),
                ..TlsClientSection::default()
            }
        }

        /// Serves `/peer`, answering how many client certificates it saw,
        /// requiring one signed by the CA.
        async fn serve(&self) -> SocketAddr {
            let section = TlsSection {
                client_auth: ClientAuth::Required,
                client_ca_file: Some(self.path("ca.pem")),
                ..TlsSection::new(self.path("server.pem"), self.path("server.key"))
            };
            let tls = Arc::new(ReloadableTls::new(&section).expect("the files read"));
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
            let address = listener.local_addr().expect("the address");
            let app = Router::new().route(
                "/peer",
                get(
                    |peers: Option<axum::Extension<PeerCertificates>>| async move {
                        peers
                            .map_or(0, |axum::Extension(peers)| peers.0.len())
                            .to_string()
                    },
                ),
            );
            tokio::spawn(crate::serve::serve_tls(
                listener,
                app,
                tls,
                Lifecycle::default(),
                std::future::pending::<()>(),
                Duration::ZERO,
                Duration::from_secs(1),
            ));
            address
        }
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).expect("the file is written");
    }

    /// The body of `GET /peer` over a connection with `client`, or `None`
    /// when no answer came.
    async fn peer(address: SocketAddr, client: &TlsClient) -> Option<String> {
        let config = client
            .client_config(RootCertStore::empty())
            .expect("the configuration");
        exchange(address, config).await
    }

    /// The body of `GET /peer` over a connection under `config`, or `None`
    /// when no answer came.
    async fn exchange(address: SocketAddr, config: rustls::ClientConfig) -> Option<String> {
        let stream = tokio::net::TcpStream::connect(address).await.ok()?;
        let name = rustls::pki_types::ServerName::try_from("localhost").expect("a name");
        let mut stream = tokio_rustls::TlsConnector::from(Arc::new(config))
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
    async fn the_identity_is_presented_and_the_extra_root_trusted() {
        let pki = Pki::new();
        let address = pki.serve().await;
        let with = TlsClient::read("remote", &pki.client(true)).expect("the material reads");
        assert!(with.has_identity());
        assert_eq!(Some("1".to_owned()), peer(address, &with).await);
        let without = TlsClient::read("remote", &pki.client(false)).expect("the roots read");
        assert_eq!(
            None,
            peer(address, &without).await,
            "the listener requires an identity"
        );
        let untrusted = TlsClient::read(
            "remote",
            &TlsClientSection {
                client_identity_file: Some(pki.path("identity.pem")),
                ..TlsClientSection::default()
            },
        )
        .expect("the identity reads");
        assert_eq!(
            None,
            peer(address, &untrusted).await,
            "the CA is no platform root"
        );
    }

    #[cfg(feature = "platform-verifier")]
    #[tokio::test]
    async fn the_platform_configuration_presents_the_identity_and_trusts_the_extra_root() {
        let pki = Pki::new();
        let address = pki.serve().await;
        let connect = |client: TlsClient| async move {
            let tls = client.platform_config().expect("the configuration");
            exchange(address, tls.into_config()).await
        };
        let with = TlsClient::read("remote", &pki.unchecked(true)).expect("the material reads");
        assert_eq!(Some("1".to_owned()), connect(with).await);
        let without = TlsClient::read("remote", &pki.unchecked(false)).expect("the roots read");
        assert_eq!(
            None,
            connect(without).await,
            "the listener requires an identity"
        );
        let untrusted = TlsClient::read(
            "remote",
            &TlsClientSection {
                client_identity_file: Some(pki.path("identity.pem")),
                ..TlsClientSection::default()
            },
        )
        .expect("the identity reads");
        assert_eq!(None, connect(untrusted).await, "the CA is no platform root");
    }

    #[cfg(feature = "platform-verifier")]
    #[tokio::test]
    async fn a_chain_to_a_checked_root_opens_no_connection_before_a_check_clears_it() {
        let pki = Pki::new();
        let address = pki.serve().await;
        let client = TlsClient::read("remote", &pki.client(true)).expect("the material reads");
        let tls = client.platform_config().expect("the configuration");
        assert!(tls.check().is_some(), "the checked roots carry a check");
        assert!(tls.unchecked_roots().is_none());
        assert_eq!(
            None,
            exchange(address, tls.into_config()).await,
            "no check cleared the chain"
        );
    }

    #[cfg(feature = "platform-verifier")]
    #[test]
    fn the_roots_without_revocation_are_named_for_the_startup_banner() {
        let pki = Pki::new();
        let client = TlsClient::read("xcpd", &pki.unchecked(false)).expect("the material reads");
        let tls = client.platform_config().expect("the configuration");
        assert!(tls.check().is_none(), "no checked roots, no check");
        let line = tls.unchecked_roots().expect("the roots are named");
        assert!(line.starts_with("xcpd: no revocation check for "), "{line}");
    }

    #[cfg(feature = "platform-verifier")]
    #[tokio::test]
    async fn reqwest_presents_the_identity_and_trusts_the_extra_root() {
        let pki = Pki::new();
        let address = pki.serve().await;
        let client = TlsClient::read("remote", &pki.unchecked(true)).expect("the material reads");
        let http = client
            .platform_config()
            .expect("the configuration")
            .apply(reqwest::Client::builder().resolve("localhost", address))
            .build()
            .expect("the client builds");
        let answer = http
            .get(format!("https://localhost:{}/peer", address.port()))
            .send()
            .await
            .expect("the call is answered");
        assert_eq!(http::StatusCode::OK, answer.status());
        assert_eq!("1", answer.text().await.expect("the body"));
    }

    #[cfg(feature = "platform-verifier")]
    #[tokio::test]
    async fn reqwest_agrees_on_x25519mlkem768_with_the_listener() {
        let pki = Pki::new();
        let section = TlsSection::new(pki.path("server.pem"), pki.path("server.key"));
        let mut config = Arc::unwrap_or_clone(
            crate::tls::server_config_of(&section, crate::tls::TlsKeys::SERVER)
                .expect("the files read"),
        );
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("the address");
        let server = tokio::spawn(async move {
            let (socket, _peer) = listener.accept().await.expect("a connection");
            let mut stream = acceptor.accept(socket).await.expect("the handshake");
            let group = stream
                .get_ref()
                .1
                .negotiated_key_exchange_group()
                .map(rustls::crypto::SupportedKxGroup::name);
            let mut request = [0_u8; 1024];
            let _read = stream.read(&mut request).await.expect("the request");
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
                .await
                .expect("the answer");
            stream.shutdown().await.expect("the close");
            group
        });
        let client = TlsClient::read("remote", &pki.unchecked(false)).expect("the material reads");
        let http = client
            .platform_config()
            .expect("the configuration")
            .apply(reqwest::Client::builder().resolve("localhost", address))
            .build()
            .expect("the client builds");
        let answer = http
            .get(format!("https://localhost:{}/", address.port()))
            .send()
            .await
            .expect("the call is answered");
        assert_eq!(http::StatusCode::NO_CONTENT, answer.status());
        assert_eq!(
            Some(rustls::NamedGroup::X25519MLKEM768),
            server.await.expect("the server ends")
        );
    }

    #[test]
    fn the_certificate_is_the_first_of_the_identity_chain() {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;

        let pki = Pki::new();
        let text = |name: &str| std::fs::read_to_string(pki.path(name)).expect("the file");
        let chained = format!(
            "{}{}{}",
            text("client.pem"),
            text("ca.pem"),
            text("client.key")
        );
        write(pki.dir.path(), "chained.pem", &chained);
        let leaf = CertificateDer::from_pem_file(pki.path("client.pem")).expect("the leaf");
        let section = TlsClientSection {
            client_identity_file: Some(pki.path("chained.pem")),
            ..TlsClientSection::default()
        };
        let client = TlsClient::read("xcpd", &section).expect("the chain reads");
        assert_eq!(Some(&leaf), client.certificate());
        let without =
            TlsClient::read("xcpd", &TlsClientSection::default()).expect("nothing to read");
        assert_eq!(None, without.certificate());
        assert!(!without.has_identity());
    }

    #[test]
    fn two_reads_are_equal_over_the_material_they_read() {
        let pki = Pki::new();
        let read = |key: &str, section: &TlsClientSection| {
            TlsClient::read(key, section).expect("the material reads")
        };
        let first = read("xcpd", &pki.client(true));
        assert_eq!(first, read("xcpd", &pki.client(true)));
        assert_eq!(
            first,
            read("pdqm", &pki.client(true)),
            "the table key takes no part"
        );
        let inline = TlsClientSection {
            client_identity: Some(Secret::new(
                std::fs::read_to_string(pki.path("identity.pem")).expect("the identity"),
            )),
            client_identity_file: None,
            ..pki.client(true)
        };
        assert_eq!(first, read("xcpd", &inline), "the same identity inline");
        assert_ne!(first, read("xcpd", &pki.client(false)), "no identity");
        assert_ne!(
            first,
            read(
                "xcpd",
                &TlsClientSection {
                    min_version: ferrohealth_config::tls::TlsVersion::Tls12,
                    ..pki.client(true)
                }
            ),
            "another minimum version"
        );

        let renewed = Pki::new();
        std::fs::copy(renewed.path("identity.pem"), pki.path("identity.pem"))
            .expect("the identity is renewed in place");
        let after_renewal = read("xcpd", &pki.client(true));
        assert_ne!(first, after_renewal, "a renewed identity at the same path");

        std::fs::copy(renewed.path("ca.pem"), pki.path("ca.pem"))
            .expect("the roots are replaced in place");
        assert_ne!(
            after_renewal,
            read("xcpd", &pki.client(true)),
            "a changed roots file at the same path"
        );
    }

    #[test]
    fn a_renewed_key_under_the_same_chain_compares_unequal() {
        let pki = Pki::new();
        let text = |name: &str| std::fs::read_to_string(pki.path(name)).expect("the file");
        let first = TlsClient::read("xcpd", &pki.client(true)).expect("the material reads");
        let other_key = KeyPair::generate().expect("a key").serialize_pem();
        write(
            pki.dir.path(),
            "identity.pem",
            &format!("{}{other_key}", text("client.pem")),
        );
        let rekeyed = TlsClient::read("xcpd", &pki.client(true)).expect("the material reads");
        assert_ne!(first, rekeyed, "the key takes part");
        let rendered = format!("{rekeyed:?}");
        assert!(!rendered.contains("PRIVATE KEY"), "{rendered}");
    }

    #[test]
    fn material_that_does_not_read_is_refused_with_its_key() {
        let pki = Pki::new();
        let both = TlsClientSection {
            client_identity: Some(Secret::new("inline")),
            client_identity_file: Some(pki.path("identity.pem")),
            ..TlsClientSection::default()
        };
        assert!(matches!(
            TlsClient::read("remote", &both),
            Err(TlsClientError::Identity(_))
        ));
        let no_key = TlsClientSection {
            client_identity_file: Some(pki.path("client.pem")),
            ..TlsClientSection::default()
        };
        assert!(matches!(
            TlsClient::read("remote", &no_key),
            Err(TlsClientError::Pem { key, .. }) if key == "remote.client_identity"
        ));
        let no_roots = TlsClientSection {
            trust_roots_file: Some(pki.path("client.key")),
            ..TlsClientSection::default()
        };
        assert!(matches!(
            TlsClient::read("remote", &no_roots),
            Err(TlsClientError::Pem { key, .. }) if key == "remote.trust_roots_file"
        ));
        let rendered = format!(
            "{:?}",
            TlsClient::read("remote", &pki.client(true)).expect("reads")
        );
        assert!(
            !rendered.contains("PRIVATE KEY") && rendered.contains("***"),
            "{rendered}"
        );
    }

    #[test]
    fn the_table_reads_from_toml() {
        let section: TlsClientSection =
            toml::from_str("client_identity_file = \"/run/secrets/id.pem\"\ntrust_roots_file = \"/etc/roots.pem\"\n")
                .expect("the table loads");
        assert_eq!(Some("/etc/roots.pem".into()), section.trust_roots_file);
        assert!(
            toml::from_str::<TlsClientSection>("roots = \"x\"\n").is_err(),
            "an unknown key is refused"
        );
    }

    #[test]
    fn the_outbound_minimum_is_tls_1_3_unless_lowered_and_never_below_1_2() {
        assert_eq!(
            ferrohealth_config::tls::TlsVersion::Tls13,
            TlsClientSection::default().min_version
        );
        let lowered: TlsClientSection =
            toml::from_str("min_version = \"1.2\"\n").expect("1.2 is taken");
        assert_eq!(
            ferrohealth_config::tls::TlsVersion::Tls12,
            lowered.min_version
        );
        let refused = toml::from_str::<TlsClientSection>("min_version = \"1.1\"\n")
            .expect_err("1.1 is refused");
        assert!(refused.to_string().contains("RFC 8996"), "{refused}");
    }
}
