// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the listener's TLS: the files it reads, the reload, the
//! client authentication and the healthcheck pin.

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
use crate::healthcheck::{Get, GetError, Outcome, check_tls, check_url};
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
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
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
    let without =
        HealthcheckTls::read(&files.section(ClientAuth::Required, false)).expect("the pin reads");
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
    let without =
        HealthcheckTls::read(&files.section(ClientAuth::Optional, false)).expect("the pin reads");
    assert_eq!(Some("1".to_owned()), peer(address, &with).await);
    assert_eq!(Some("0".to_owned()), peer(address, &without).await);
}

#[tokio::test]
async fn without_client_auth_no_certificate_is_asked() {
    let files = Files::new();
    let address = start(&files.section(ClientAuth::None, false)).await;
    let pin = HealthcheckTls::read(&files.section(ClientAuth::None, false)).expect("the pin reads");
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

#[tokio::test]
async fn a_get_over_the_pinned_tls_reads_the_status_and_the_body() {
    let files = Files::new();
    let address = start(&files.section(ClientAuth::Required, false)).await;
    let with = HealthcheckTls::read(&files.section(ClientAuth::Required, true))
        .expect("the identity reads");
    let answer = Get::new(address, "/peer", Duration::from_secs(5), 64)
        .with_tls(with.clone())
        .send()
        .await
        .expect("an answer");
    assert_eq!(http::StatusCode::OK, answer.status());
    assert_eq!(b"1", answer.body(), "the identity reached the handler");
    let readiness = Get::new(address, "/health/readiness", Duration::from_secs(5), 65_536)
        .with_tls(with)
        .send()
        .await
        .expect("an answer");
    assert_eq!(http::StatusCode::OK, readiness.status());
    assert!(
        serde_json::from_slice::<serde_json::Value>(readiness.body()).is_ok(),
        "the readiness document is read whole"
    );
}

#[tokio::test]
async fn a_get_over_the_pinned_tls_refuses_a_listener_with_another_certificate() {
    let served = Files::new();
    let address = start(&served.section(ClientAuth::None, false)).await;
    let other = Files::new();
    let pin = HealthcheckTls::read(&other.section(ClientAuth::None, false)).expect("the pin reads");
    let error = Get::new(address, "/peer", Duration::from_secs(5), 64)
        .with_tls(pin.clone())
        .send()
        .await
        .expect_err("the listener presents a certificate the pin does not hold");
    assert!(matches!(error, GetError::Handshake(_)), "{error:?}");
    assert!(
        matches!(
            check_tls(address, "", Duration::from_secs(5), &pin).await,
            Outcome::Failed(reason) if reason.starts_with("the TLS handshake failed")
        ),
        "the check refuses it too"
    );
}

#[tokio::test]
async fn a_get_over_the_pinned_tls_refuses_a_body_past_its_bound() {
    let files = Files::new();
    let address = start(&files.section(ClientAuth::None, false)).await;
    let pin = HealthcheckTls::read(&files.section(ClientAuth::None, false)).expect("the pin reads");
    let error = Get::new(address, "/health/readiness", Duration::from_secs(5), 4)
        .with_tls(pin.clone())
        .send()
        .await
        .expect_err("the readiness document is longer than four bytes");
    assert!(
        matches!(
            error,
            GetError::TooLong {
                status: http::StatusCode::OK,
                limit: 4
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        Outcome::Healthy,
        check_tls(address, "", Duration::from_secs(5), &pin).await,
        "the check reads no body, so no bound refuses it"
    );
}

#[tokio::test]
async fn a_get_over_the_pinned_tls_gives_up_at_its_timeout() {
    let files = Files::new();
    let pin = HealthcheckTls::read(&files.section(ClientAuth::None, false)).expect("the pin reads");
    let silent = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = silent.local_addr().expect("the address");
    let held = tokio::spawn(async move {
        let (stream, _peer) = silent.accept().await.expect("a connection");
        std::future::pending::<()>().await;
        drop(stream);
    });
    let timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let error = Get::new(address, "/peer", timeout, 64)
        .with_tls(pin.clone())
        .send()
        .await
        .expect_err("the listener never answers the handshake");
    assert!(
        matches!(error, GetError::Timeout(t) if t == timeout),
        "{error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it gave up in time"
    );
    assert_eq!(
        Outcome::Timeout,
        check_tls(address, "", timeout, &pin).await
    );
    held.abort();
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
    let tls = ReloadableTls::new(&files.section(ClientAuth::None, false)).expect("the files read");
    let before = tls.current();
    let fresh =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("a certificate");
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
