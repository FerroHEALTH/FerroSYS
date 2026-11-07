// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The bindings of a sender-constrained token in the load phase: the TLS
//! client certificate of every connection (RFC 8705 §3) against a server
//! that requires one and records it, and a fresh `DPoP` proof per request
//! (RFC 9449 §4) against a server that checks each proof.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ferrohealth_perf::credentials::Credentials;
use ferrohealth_perf::load::drive;
use ferrohealth_perf::plan::{Plan, PlanError};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use rustls::RootCertStore;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use url::Url;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const LOAD: &str = "[load]\nrate = 10.0\nwarmup_secs = 0\nduration_secs = 2\n";

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A CA, a `localhost` server certificate and a client certificate of it.
struct Pki {
    ca: CertificateDer<'static>,
    ca_pem: String,
    server_chain: Vec<CertificateDer<'static>>,
    server_key: PrivateKeyDer<'static>,
    client: CertificateDer<'static>,
    client_pem: String,
}

fn pki() -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let server_key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec!["localhost".to_owned()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let client_key = KeyPair::generate().unwrap();
    let client = CertificateParams::new(vec!["perf-client.example".to_owned()])
        .unwrap()
        .signed_by(&client_key, &issuer)
        .unwrap();
    Pki {
        ca: ca.der().clone(),
        ca_pem: ca.pem(),
        server_chain: vec![server.der().clone()],
        server_key: PrivateKeyDer::from_pem_slice(server_key.serialize_pem().as_bytes()).unwrap(),
        client: client.der().clone(),
        client_pem: format!("{}{}", client.pem(), client_key.serialize_pem()),
    }
}

/// Serves TLS on a loopback port, requiring a client certificate the CA
/// issued, answering `200` to every request and recording the certificate
/// each connection presented.
async fn listen(pki: &Pki, presented: &Arc<Mutex<Vec<Vec<u8>>>>) -> String {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = RootCertStore::empty();
    roots.add(pki.ca.clone()).unwrap();
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_single_cert(pki.server_chain.clone(), pki.server_key.clone_key())
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let presented = Arc::clone(presented);
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let presented = Arc::clone(&presented);
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(stream).await else {
                    return;
                };
                if let Some(chain) = tls.get_ref().1.peer_certificates() {
                    let first = chain
                        .first()
                        .map(|c| c.as_ref().to_vec())
                        .unwrap_or_default();
                    presented.lock().unwrap().push(first);
                }
                let mut buffer = [0_u8; 4096];
                if matches!(tls.read(&mut buffer).await, Ok(read) if read > 0) {
                    let answered = tls
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .await;
                    if answered.is_ok() {
                        let _closed = tls.shutdown().await;
                    }
                }
            });
        }
    });
    format!("https://localhost:{port}")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_identity_is_presented_on_every_connection() {
    let pki = pki();
    let presented = Arc::new(Mutex::new(Vec::new()));
    let base = listen(&pki, &presented).await;
    let dir = tempfile::tempdir().unwrap();
    let identity = write(dir.path(), "client.pem", &pki.client_pem);
    let roots = write(dir.path(), "ca.pem", &pki.ca_pem);
    let text = format!(
        "{LOAD}client_identity_file = '{identity}'\ntrust_roots_file = '{roots}'\n\
         [[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/ok\"\nstatus = 200\n"
    );
    let plan_path = write(dir.path(), "plan.toml", &text);
    let plan = Plan::read(Path::new(&plan_path)).unwrap();
    let credentials = Credentials::load(&plan).unwrap();
    let client = credentials
        .apply(reqwest::Client::builder().no_proxy())
        .build()
        .unwrap();
    let outcome = drive(
        &plan,
        &Url::parse(&base).unwrap(),
        &client,
        &credentials,
        Instant::now(),
    )
    .await
    .unwrap();
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(
        20, outcome.groups["read"].answered,
        "every request answered"
    );
    let seen = presented.lock().unwrap().clone();
    assert_eq!(
        20,
        seen.len(),
        "the server closes each connection, so one per request"
    );
    assert!(
        seen.iter().all(|der| der.as_slice() == pki.client.as_ref()),
        "every connection presented the client certificate"
    );

    // Without the identity the server refuses every connection.
    let anonymous =
        Plan::parse(&text.replace(&format!("client_identity_file = '{identity}'\n"), "")).unwrap();
    let credentials = Credentials::load(&anonymous).unwrap();
    let client = credentials
        .apply(reqwest::Client::builder().no_proxy())
        .build()
        .unwrap();
    let outcome = drive(
        &anonymous,
        &Url::parse(&base).unwrap(),
        &client,
        &credentials,
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(0, outcome.groups["read"].answered);
    assert!(!outcome.failures.is_empty());
}

/// A server that checks every `DPoP` proof under RFC 9449 §4.3 and records
/// its `jti`: `200` for a valid proof, `400` for any other.
#[derive(Clone)]
struct ProofChecker {
    jtis: Arc<Mutex<BTreeSet<String>>>,
    refusals: Arc<Mutex<Vec<String>>>,
    token: &'static str,
    // NOTE: no specification governs this: our own design; wiremock names the request URL without its host, so the checker holds the server's base.
    base: String,
}

impl ProofChecker {
    fn check(&self, request: &Request) -> Result<(), String> {
        let header = |name: &str| {
            request
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .ok_or(format!("no {name}"))
        };
        let proof = header("dpop")?;
        let [head, claims, signature]: [&str; 3] = proof
            .split('.')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_parts| "not three parts".to_owned())?;
        let decode = |part: &str| -> Result<serde_json::Value, String> {
            let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|e| e.to_string())?;
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        };
        let (head, body) = (decode(head)?, decode(claims)?);
        if head["typ"] != "dpop+jwt" || head["alg"] != "ES256" {
            return Err(format!("header {head}"));
        }
        let coordinate =
            |name: &str| URL_SAFE_NO_PAD.decode(head["jwk"][name].as_str().unwrap_or_default());
        let mut point = vec![4_u8];
        point.extend(coordinate("x").map_err(|e| e.to_string())?);
        point.extend(coordinate("y").map_err(|e| e.to_string())?);
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|e| e.to_string())?;
        let signed = proof
            .rsplit_once('.')
            .map(|(input, _)| input)
            .unwrap_or_default();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &point)
            .verify(signed.as_bytes(), &signature)
            .map_err(|_bad| "the signature does not verify".to_owned())?;
        let htu = format!("{}{}", self.base, request.url.path());
        if body["htm"] != request.method.as_str() || body["htu"] != htu.as_str() {
            return Err(format!(
                "htm or htu {body} against {} {htu}",
                request.method
            ));
        }
        let now = jiff::Timestamp::now().as_second();
        if (now - body["iat"].as_i64().unwrap_or_default()).abs() > 5 {
            return Err(format!("iat {body}"));
        }
        let ath = URL_SAFE_NO_PAD.encode(digest(&SHA256, self.token.as_bytes()).as_ref());
        if body["ath"] != ath.as_str() {
            return Err(format!("ath {body}"));
        }
        if header("authorization")? != format!("DPoP {}", self.token) {
            return Err("the token is not under the DPoP scheme".to_owned());
        }
        let jti = body["jti"].as_str().unwrap_or_default().to_owned();
        if !self.jtis.lock().unwrap().insert(jti) {
            return Err("a jti was spent twice".to_owned());
        }
        Ok(())
    }
}

impl Respond for ProofChecker {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        match self.check(request) {
            Ok(()) => ResponseTemplate::new(200),
            Err(reason) => {
                self.refusals.lock().unwrap().push(reason.clone());
                ResponseTemplate::new(400).set_body_string(reason)
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_request_carries_a_fresh_proof_that_verifies() {
    let server = MockServer::start().await;
    let checker = ProofChecker {
        jtis: Arc::new(Mutex::new(BTreeSet::new())),
        refusals: Arc::new(Mutex::new(Vec::new())),
        token: "a-bound-token",
        base: server.uri(),
    };
    Mock::given(wiremock::matchers::any())
        .respond_with(checker.clone())
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let document = EcdsaKeyPair::generate_pkcs8(
        &ECDSA_P256_SHA256_FIXED_SIGNING,
        &aws_lc_rs::rand::SystemRandom::new(),
    )
    .unwrap();
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(document.as_ref())
    );
    let key = write(dir.path(), "dpop.pem", &pem);
    let text = format!(
        "{LOAD}[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/fhir/Patient?x=1\"\n\
         status = 200\ndpop_key_file = '{key}'\nheaders = {{ authorization = \"Bearer a-bound-token\" }}\n\
         [[requests]]\ngroup = \"write\"\nmethod = \"POST\"\npath = \"/fhir/Patient\"\nstatus = 200\n\
         body = '{{}}'\ndpop_key_file = '{key}'\nheaders = {{ authorization = \"Bearer a-bound-token\" }}\n"
    );
    let plan_path = write(dir.path(), "plan.toml", &text);
    let plan = Plan::read(Path::new(&plan_path)).unwrap();
    let credentials = Credentials::load(&plan).unwrap();
    let outcome = drive(
        &plan,
        &Url::parse(&server.uri()).unwrap(),
        &reqwest::Client::new(),
        &credentials,
        Instant::now(),
    )
    .await
    .unwrap();
    assert!(
        outcome.failures.is_empty(),
        "{:?}: {:?}",
        outcome.failures,
        checker.refusals.lock().unwrap().first()
    );
    assert_eq!(20, outcome.sent);
    assert_eq!(
        20,
        checker.jtis.lock().unwrap().len(),
        "a new jti for every request"
    );
}

#[test]
fn a_credential_file_that_cannot_be_read_or_parsed_refuses_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let not_a_key = write(dir.path(), "not-a-key.pem", "nothing here\n");
    for (key, line) in [
        (
            "client_identity_file",
            format!(
                "client_identity_file = '{}'",
                dir.path().join("absent.pem").display()
            ),
        ),
        (
            "trust_roots_file",
            format!("trust_roots_file = '{not_a_key}'"),
        ),
    ] {
        let text = format!(
            "{LOAD}{line}\n[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/\"\nstatus = 200\n"
        );
        let path = write(dir.path(), "plan.toml", &text);
        assert!(
            matches!(Plan::read(Path::new(&path)), Err(PlanError::Credential(_))),
            "{key}"
        );
    }
    let text = format!(
        "{LOAD}[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/\"\nstatus = 200\n\
         dpop_key_file = '{not_a_key}'\n"
    );
    let path = write(dir.path(), "plan.toml", &text);
    assert!(matches!(
        Plan::read(Path::new(&path)),
        Err(PlanError::Credential(_))
    ));
}
