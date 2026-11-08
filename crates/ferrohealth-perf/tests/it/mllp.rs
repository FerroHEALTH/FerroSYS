// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! HL7 v2 over MLLP as load: an in-process listener that frames its
//! acknowledgments by MLLP Release 1, plain and over TLS, driven directly,
//! by `run --update` and by `ab`.

use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use ferrohealth_perf::credentials::Credentials;
use ferrohealth_perf::load::{Endpoints, drive};
use ferrohealth_perf::plan::Plan;
use rustls::RootCertStore;
use rustls::server::WebPkiClientVerifier;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use url::Url;

use crate::bindings::{pki, write};
use crate::support::{self, Planted};

const ACCEPTED: &str = "MSH|^~\\&|PERF|FAC|PIX|FAC|20261108100000||ADT^A01^ADT_A01|FIXED|P|2.5\n\
                        EVN|A01|20261108100000\nPID|||P-1^^^&1.2.3&ISO||DOE^JANE\n";

const REJECTED: &str = "MSH|^~\\&|PERF|FAC|PIX|FAC|20261108100000||ADT^A01^ADT_A01|FIXED|P|2.5\n\
                        EVN|A01|20261108100000\nPID|||REJECT^^^&1.2.3&ISO||DOE^JOHN\n";

/// What the listener saw: each message's MSH-10, and the connections.
#[derive(Debug, Default)]
struct Seen {
    control_ids: Vec<String>,
    connections: usize,
}

/// Answers every framed message on `stream` with an acknowledgment that
/// echoes its MSH-10 after 20 ms: `AE` when it holds `REJECT`, `AA`
/// otherwise.
async fn answer<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, seen: Arc<Mutex<Seen>>) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        while let Some(end) = buffer.windows(2).position(|pair| pair == [0x1c, 0x0d]) {
            let frame: Vec<u8> = buffer.drain(..end + 2).collect();
            assert_eq!(0x0b, frame[0], "the start block");
            let text = String::from_utf8(frame[1..end].to_vec()).unwrap();
            let header = text.split('\r').next().unwrap();
            let control_id = header.split('|').nth(9).unwrap().to_owned();
            let code = if text.contains("REJECT") { "AE" } else { "AA" };
            // A steady 20 ms answer keeps the window steady on a shared runner.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            seen.lock().unwrap().control_ids.push(control_id.clone());
            let ack = format!(
                "\x0bMSH|^~\\&|PIX|FAC|PERF|FAC|20261108100000||ACK^A01^ACK|R-{control_id}|P|2.5\r\
                 MSA|{code}|{control_id}\r\x1c\r"
            );
            if stream.write_all(ack.as_bytes()).await.is_err() {
                return;
            }
        }
    }
}

/// Listens on a loopback port, with TLS requiring a client certificate when
/// `tls` is given; returns its address.
async fn listen(tls: Option<TlsAcceptor>, seen: &Arc<Mutex<Seen>>) -> String {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::clone(seen);
    tokio::spawn(async move {
        while let Ok((stream, _peer)) = listener.accept().await {
            seen.lock().unwrap().connections += 1;
            let seen = Arc::clone(&seen);
            let tls = tls.clone();
            tokio::spawn(async move {
                match tls {
                    None => answer(stream, seen).await,
                    Some(acceptor) => {
                        if let Ok(stream) = acceptor.accept(stream).await {
                            answer(stream, seen).await;
                        }
                    }
                }
            });
        }
    });
    format!("localhost:{port}")
}

fn messages(dir: &Path) -> String {
    let accepted = write(dir, "a01.hl7", ACCEPTED);
    let rejected = write(dir, "rejected.hl7", REJECTED);
    format!(
        "[[messages]]\ngroup = \"iti-8\"\nfile = '{accepted}'\n\
         [[messages]]\ngroup = \"iti-8\"\nfile = '{accepted}'\n\
         [[messages]]\ngroup = \"rejected\"\nfile = '{rejected}'\n"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn each_message_waits_for_its_ack_and_an_ae_is_a_failure() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let address = listen(None, &seen).await;
    let dir = tempfile::tempdir().unwrap();
    let plan = Plan::parse(&format!(
        "[load]\nrate = 30.0\nwarmup_secs = 0\nduration_secs = 2\n\
         [mllp]\naddress = '{address}'\n{}",
        messages(dir.path())
    ))
    .unwrap();
    let endpoints = Endpoints::new(&plan, Url::parse("http://127.0.0.1:9").unwrap(), None).unwrap();
    assert_eq!(Some(address.as_str()), endpoints.mllp_address());
    let outcome = drive(
        &plan,
        &endpoints,
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(60, outcome.sent);
    assert_eq!(
        40, outcome.groups["iti-8"].answered,
        "{:?}",
        outcome.failures
    );
    assert_eq!(0, outcome.groups["rejected"].answered);
    assert_eq!(20, outcome.failures["rejected"]["ack AE"]);
    assert!(
        !outcome.failures.contains_key("iti-8"),
        "{:?}",
        outcome.failures
    );
    let seen = seen.lock().unwrap();
    assert_eq!(60, seen.control_ids.len());
    let distinct: std::collections::BTreeSet<&String> = seen.control_ids.iter().collect();
    assert_eq!(60, distinct.len(), "a fresh MSH-10 per send");
    assert!(
        seen.control_ids.iter().all(|id| id.len() == 20),
        "{:?}",
        seen.control_ids
    );
    assert!(
        seen.connections < 60,
        "connections are kept: {} for 60 messages",
        seen.connections
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tls_listener_takes_the_plans_identity_and_roots() {
    let pki = pki();
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
    let seen = Arc::new(Mutex::new(Seen::default()));
    let address = listen(Some(TlsAcceptor::from(Arc::new(config))), &seen).await;
    let dir = tempfile::tempdir().unwrap();
    let identity = write(dir.path(), "client.pem", &pki.client_pem);
    let ca = write(dir.path(), "ca.pem", &pki.ca_pem);
    let accepted = write(dir.path(), "a01.hl7", ACCEPTED);
    let text = format!(
        "[load]\nrate = 10.0\nwarmup_secs = 0\nduration_secs = 2\n\
         client_identity_file = '{identity}'\ntrust_roots_file = '{ca}'\n\
         [mllp]\naddress = '{address}'\ntls = true\n\
         [[messages]]\ngroup = \"iti-8\"\nfile = '{accepted}'\n"
    );
    let plan_path = write(dir.path(), "plan.toml", &text);
    let plan = Plan::read(Path::new(&plan_path)).unwrap();
    let endpoints = Endpoints::new(&plan, Url::parse("http://127.0.0.1:9").unwrap(), None).unwrap();
    let outcome = drive(
        &plan,
        &endpoints,
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
    assert_eq!(20, outcome.groups["iti-8"].answered);

    // Without the client identity the listener refuses the handshake.
    let anonymous =
        Plan::parse(&text.replace(&format!("client_identity_file = '{identity}'\n"), "")).unwrap();
    let endpoints =
        Endpoints::new(&anonymous, Url::parse("http://127.0.0.1:9").unwrap(), None).unwrap();
    let outcome = drive(
        &anonymous,
        &endpoints,
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(0, outcome.groups["iti-8"].answered);
    assert!(!outcome.failures.is_empty());
}

fn perf(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ferrohealth-perf"))
        .args(args)
        .env_remove("GITHUB_STEP_SUMMARY")
        .env_remove("FERROHEALTH_PERF_DATABASE_URL")
        .env_remove("FERROHEALTH_PERF_BASE_DATABASE_URL")
        .env_remove("FERROHEALTH_PERF_HEAD_DATABASE_URL")
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_load_the_run_and_the_ab_mode() {
    let (base_seen, head_seen) = (Arc::default(), Arc::default());
    let base = listen(None, &base_seen).await;
    let head = listen(None, &head_seen).await;
    let process = Planted::start("quiet", None);
    let pid = process.pid().to_string();
    let dir = tempfile::tempdir().unwrap();
    let accepted = write(dir.path(), "a01.hl7", ACCEPTED);
    let plan = write(
        dir.path(),
        "plan.toml",
        &support::plan(&format!(
            "[[messages]]\ngroup = \"iti-8\"\nfile = '{accepted}'\n"
        )),
    );
    let baseline = dir.path().join("baseline.toml");
    let out = dir.path().join("reports");
    let run = perf(&[
        "run",
        "--plan",
        &plan,
        "--pid",
        &pid,
        "--base-url",
        "http://127.0.0.1:9",
        "--mllp-address",
        &head,
        "--baseline",
        baseline.to_str().unwrap(),
        "--out-dir",
        out.to_str().unwrap(),
        "--update",
        "--runs",
        "2",
    ]);
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        Some(0),
        run.status.code(),
        "{stdout}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(stdout.contains("| iti-8 |"), "{stdout}");

    let ab = perf(&[
        "ab",
        "--plan",
        &plan,
        "--base-url",
        "http://127.0.0.1:9",
        "--base-pid",
        &pid,
        "--base-mllp-address",
        &base,
        "--head-url",
        "http://127.0.0.1:9",
        "--head-pid",
        &pid,
        "--head-mllp-address",
        &head,
        "--pairs",
        "2",
        "--out-dir",
        out.to_str().unwrap(),
    ]);
    let stdout = String::from_utf8_lossy(&ab.stdout);
    assert!(stdout.contains("| iti-8.p50_ms |"), "{stdout}");
    assert_eq!(
        Some(0),
        ab.status.code(),
        "{stdout}{}",
        String::from_utf8_lossy(&ab.stderr)
    );
    let base_count = base_seen.lock().unwrap().control_ids.len();
    assert!(
        base_count >= 2 * 150,
        "the base's listener took its phases: {base_count}"
    );
}
