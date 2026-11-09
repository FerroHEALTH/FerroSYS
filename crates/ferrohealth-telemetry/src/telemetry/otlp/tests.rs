// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The dial of an `https://` collector: it speaks the handed TLS, so the
//! client identity, the trust roots and the minimum version hold.

use std::sync::Arc;

use http::Uri;
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use tokio::net::TcpListener;

use super::Dial;
use crate::telemetry::InitError;

/// A CA, a collector certificate for `localhost` and a client identity,
/// each signed by the CA.
struct Pki {
    ca: CertificateDer<'static>,
    server: (CertificateDer<'static>, PrivateKeyDer<'static>),
    client: (CertificateDer<'static>, PrivateKeyDer<'static>),
}

impl Pki {
    fn new() -> Self {
        let _installed = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let ca_key = KeyPair::generate().expect("a CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).expect("a CA certificate");
        let issuer = Issuer::new(ca_params, ca_key);
        let leaf = |name: &str| {
            let key = KeyPair::generate().expect("a key");
            let certificate = CertificateParams::new(vec![name.to_owned()])
                .expect("parameters")
                .signed_by(&key, &issuer)
                .expect("a certificate");
            (
                certificate.der().clone(),
                PrivateKeyDer::from_pem_slice(key.serialize_pem().as_bytes()).expect("the key"),
            )
        };
        Self {
            ca: ca.der().clone(),
            server: leaf("localhost"),
            client: leaf("client.example"),
        }
    }

    fn roots(&self) -> RootCertStore {
        let mut roots = RootCertStore::empty();
        roots.add(self.ca.clone()).expect("the CA");
        roots
    }

    /// A collector that requires a client certificate signed by the CA and
    /// speaks `versions`, answering the handshake and nothing more.
    async fn collector(&self, versions: &[&'static rustls::SupportedProtocolVersion]) -> u16 {
        let verifier = WebPkiClientVerifier::builder(Arc::new(self.roots()))
            .build()
            .expect("the verifier");
        let mut config = ServerConfig::builder_with_protocol_versions(versions)
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![self.server.0.clone()], self.server.1.clone_key())
            .expect("the server configuration");
        config.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let port = listener.local_addr().expect("the address").port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let _handshake = acceptor.accept(tcp).await;
                });
            }
        });
        port
    }

    /// The client configuration a product builds, presenting the identity
    /// when `identity` is set, at `versions`.
    fn client(
        &self,
        identity: bool,
        versions: &[&'static rustls::SupportedProtocolVersion],
    ) -> ClientConfig {
        let builder = ClientConfig::builder_with_protocol_versions(versions)
            .with_root_certificates(self.roots());
        if identity {
            builder
                .with_client_auth_cert(vec![self.client.0.clone()], self.client.1.clone_key())
                .expect("the identity")
        } else {
            builder.with_no_client_auth()
        }
    }
}

fn uri(port: u16) -> Uri {
    format!("https://localhost:{port}").parse().expect("a URI")
}

#[tokio::test]
async fn the_dial_presents_the_identity_and_offers_h2() {
    let pki = Pki::new();
    let port = pki.collector(rustls::ALL_VERSIONS).await;
    let dial =
        Dial::new(&uri(port), &pki.client(true, &[&rustls::version::TLS13])).expect("the dial");
    let stream = dial.connect().await.expect("the handshake completes");
    let (_, session) = stream.inner().get_ref();
    assert_eq!(Some(&b"h2"[..]), session.alpn_protocol());
    assert_eq!(
        Some(rustls::ProtocolVersion::TLSv1_3),
        session.protocol_version()
    );
}

#[tokio::test]
async fn a_dial_without_the_identity_is_refused_by_the_collector() {
    let pki = Pki::new();
    let port = pki.collector(rustls::ALL_VERSIONS).await;
    let dial = Dial::new(&uri(port), &pki.client(false, rustls::ALL_VERSIONS)).expect("the dial");
    let refused = match dial.connect().await {
        Err(error) => Some(error),
        Ok(stream) => {
            // NOTE: RFC 8446 §4.4.2, a TLS 1.3 server reports a missing client certificate
            // after the client's Finished, so the refusal arrives on the first read.
            let mut stream = stream.into_inner();
            let mut byte = [0_u8; 1];
            tokio::io::AsyncReadExt::read(&mut stream, &mut byte)
                .await
                .err()
        }
    };
    assert!(
        refused.is_some(),
        "the collector refuses a client with no certificate"
    );
}

#[tokio::test]
async fn the_minimum_version_of_the_handed_tls_holds() {
    let pki = Pki::new();
    let port = pki.collector(&[&rustls::version::TLS12]).await;
    let dial =
        Dial::new(&uri(port), &pki.client(true, &[&rustls::version::TLS13])).expect("the dial");
    assert!(
        dial.connect().await.is_err(),
        "a TLS 1.3 client refuses a TLS 1.2 collector"
    );
}

#[test]
fn a_host_no_certificate_can_name_is_refused() {
    let pki = Pki::new();
    let config = pki.client(true, rustls::ALL_VERSIONS);
    let bare: Uri = "/no-host".parse().expect("a URI");
    assert!(matches!(
        Dial::new(&bare, &config),
        Err(InitError::ServerName)
    ));
    let literal = Dial::new(&"https://[::1]:4317".parse().expect("a URI"), &config);
    assert!(literal.is_ok(), "an IP literal is a server name");
}
