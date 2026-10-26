// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The TLS material of a product's outbound HTTPS calls.
//!
//! A table that names a remote service takes `client_identity`, inline, or
//! `client_identity_file`: the client certificate chain and its private key
//! in one PEM, presented for mutual TLS and held as a secret; and
//! `trust_roots_file`, a PEM bundle of trust roots beside the platform's.
//! [`TlsClient::read`] reads every file once; [`TlsClient::client_config`]
//! gives a rustls configuration over the platform roots the caller hands
//! it, and `TlsClient::apply` (feature `reqwest`) sets the same material on
//! a `reqwest::ClientBuilder`. No specification governs the keys: our own
//! design.

use std::path::PathBuf;

use ferrohealth_secret::secret::Secret;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};

/// The outbound TLS keys of one table.
///
/// `Debug` and serialization show the inline identity as `***`, as
/// [`Secret`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsClientSection {
    /// The client certificate chain and its private key, in one PEM.
    pub client_identity: Option<Secret>,
    /// A file holding `client_identity`.
    pub client_identity_file: Option<PathBuf>,
    /// A PEM bundle of trust roots beside the platform's.
    pub trust_roots_file: Option<PathBuf>,
}

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
    /// The HTTP client refused the identity or a root.
    #[cfg(feature = "reqwest")]
    #[error("{key} was refused by the HTTP client")]
    Client {
        /// The key the material was read from.
        key: String,
        /// What reqwest reported.
        #[source]
        source: reqwest::Error,
    },
}

/// The client identity: its PEM, as reqwest takes it, and its parts.
struct Identity {
    #[cfg(feature = "reqwest")]
    pem: Secret,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

/// The outbound TLS material of one table, every file read.
pub struct TlsClient {
    key: String,
    identity: Option<Identity>,
    roots: Vec<CertificateDer<'static>>,
}

impl std::fmt::Debug for TlsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClient")
            .field("key", &self.key)
            .field("identity", &self.identity.as_ref().map(|_identity| "***"))
            .field("roots", &self.roots.len())
            .finish()
    }
}

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
            Ok::<_, TlsClientError>(Identity {
                #[cfg(feature = "reqwest")]
                pem,
                chain,
                key,
            })
        })
        .transpose()?;
        let roots = match &section.trust_roots_file {
            None => Vec::new(),
            Some(path) => {
                let roots_key = format!("{key}.trust_roots_file");
                let text = std::fs::read(path).map_err(|source| TlsClientError::Read {
                    key: roots_key.clone(),
                    path: path.clone(),
                    source,
                })?;
                certificates(&text).ok_or(TlsClientError::Pem {
                    key: roots_key,
                    holds: "certificate",
                })?
            }
        };
        Ok(Self {
            key: key.to_owned(),
            identity,
            roots,
        })
    }

    /// Whether a client identity is presented.
    #[must_use]
    pub const fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// The rustls configuration that trusts `platform_roots` and the extra
    /// roots, and presents the client identity when one is set.
    ///
    /// The protocol versions and cipher suites are the listener's: TLS 1.3,
    /// and TLS 1.2 with the four suites RFC 9325 §4.2 recommends.
    ///
    /// # Errors
    ///
    /// Returns [`TlsClientError::Refused`] when rustls refuses a root or the
    /// identity.
    pub fn client_config(
        &self,
        mut platform_roots: RootCertStore,
    ) -> Result<ClientConfig, TlsClientError> {
        for root in &self.roots {
            platform_roots
                .add(root.clone())
                .map_err(|source| self.refused("trust_roots_file", source))?;
        }
        let builder = ClientConfig::builder_with_provider(crate::tls::provider())
            .with_safe_default_protocol_versions()
            .map_err(|source| self.refused("client_identity", source))?
            .with_root_certificates(platform_roots);
        match &self.identity {
            None => Ok(builder.with_no_client_auth()),
            Some(identity) => builder
                .with_client_auth_cert(identity.chain.clone(), identity.key.clone_key())
                .map_err(|source| self.refused("client_identity", source)),
        }
    }

    /// Sets the extra roots, beside the platform's, and the client identity
    /// on `builder`.
    ///
    /// # Errors
    ///
    /// Returns [`TlsClientError::Client`] when reqwest refuses a root or the
    /// identity.
    #[cfg(feature = "reqwest")]
    pub fn apply(
        &self,
        mut builder: reqwest::ClientBuilder,
    ) -> Result<reqwest::ClientBuilder, TlsClientError> {
        for root in &self.roots {
            let root = reqwest::Certificate::from_der(root)
                .map_err(|source| self.client("trust_roots_file", source))?;
            builder = builder.add_root_certificate(root);
        }
        if let Some(identity) = &self.identity {
            let identity = reqwest::Identity::from_pem(identity.pem.expose().as_bytes())
                .map_err(|source| self.client("client_identity", source))?;
            builder = builder.identity(identity);
        }
        Ok(builder)
    }

    fn refused(&self, name: &str, source: rustls::Error) -> TlsClientError {
        TlsClientError::Refused {
            key: format!("{}.{name}", self.key),
            source,
        }
    }

    #[cfg(feature = "reqwest")]
    fn client(&self, name: &str, source: reqwest::Error) -> TlsClientError {
        TlsClientError::Client {
            key: format!("{}.{name}", self.key),
            source,
        }
    }
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

    use super::{TlsClient, TlsClientError, TlsClientSection};
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

    #[cfg(feature = "reqwest")]
    #[tokio::test]
    async fn reqwest_presents_the_identity_and_trusts_the_extra_root() {
        let pki = Pki::new();
        let address = pki.serve().await;
        let client = TlsClient::read("remote", &pki.client(true)).expect("the material reads");
        let http = client
            .apply(reqwest::Client::builder().resolve("localhost", address))
            .expect("reqwest takes the material")
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
}
