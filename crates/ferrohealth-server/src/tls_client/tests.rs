// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the outbound TLS material and its configurations.

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
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
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
    let without = TlsClient::read("xcpd", &TlsClientSection::default()).expect("nothing to read");
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
    let section: TlsClientSection = toml::from_str(
        "client_identity_file = \"/run/secrets/id.pem\"\ntrust_roots_file = \"/etc/roots.pem\"\n",
    )
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
    let refused =
        toml::from_str::<TlsClientSection>("min_version = \"1.1\"\n").expect_err("1.1 is refused");
    assert!(refused.to_string().contains("RFC 8996"), "{refused}");
}
