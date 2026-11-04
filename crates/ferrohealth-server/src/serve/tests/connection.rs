// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One connection: its client address, the head and first-byte bounds, the
//! body left unread, and the accept loop under errors.

use std::time::Duration;

use axum::Router;
use axum::routing::get;
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use tokio::net::TcpListener;

use crate::health::Health;
use crate::healthcheck::{Outcome, check};
use crate::section::ServerSection;
use crate::serve::Drained;
use crate::stack::{StackPolicy, app};

use super::{certificate, closed_after, fetch, post_then_get, short_head_bound, start, start_run};

#[tokio::test]
async fn a_request_over_a_connection_names_its_client_address() {
    use crate::client_address::ClientAddress;
    let product = Router::new().route(
        "/who",
        get(
            |axum::Extension(ClientAddress(client)): axum::Extension<ClientAddress>| async move {
                client.to_string()
            },
        ),
    );
    let (address, _lifecycle, _stop, _served) =
        start(product, Duration::ZERO, Duration::from_secs(1)).await;
    assert!(fetch(address, "/who").await.ends_with("127.0.0.1"));
}

#[tokio::test]
async fn a_client_that_never_finishes_its_head_is_disconnected() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let section = ServerSection {
        header_read_timeout_ms: std::num::NonZeroU64::new(200).expect("not zero"),
        ..ServerSection::default()
    };
    let app = app(
        Router::new(),
        Health::new(Registry::default(), lifecycle.clone()),
        &section,
        StackPolicy::default(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        crate::serve::run(listener, app, &section, lifecycle, async move {
            let _received = stopped.await;
        })
        .await
    });
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connects");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("writes half a head");
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    assert!(
        read.is_ok(),
        "the server closes the connection instead of waiting for the head"
    );
    stop.send(()).expect("the server listens for the signal");
    let _drained = served.await.expect("the task ends");
}

#[tokio::test]
async fn an_accepted_connection_sends_without_delay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let _client = tokio::net::TcpStream::connect(address)
        .await
        .expect("a connection");
    let (accepted, _peer) = listener.accept().await.expect("accepted");
    assert!(!accepted.nodelay().expect("the option reads"));
    crate::serve::no_delay(&accepted);
    assert!(
        accepted.nodelay().expect("the option reads"),
        "every accepted connection, plain or TLS, has TCP_NODELAY set"
    );
}

#[tokio::test]
async fn a_connection_that_sends_no_byte_is_closed_at_the_header_read_timeout() {
    let (address, stop, served) = start_run(Router::new(), short_head_bound()).await;
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connects");
    let closed = closed_after(&mut stream).await;
    assert!(
        closed.is_some_and(|after| after >= Duration::from_millis(150)),
        "a silent connection is closed once the bound from accept passes: {closed:?}"
    );
    assert_eq!(
        Outcome::Healthy,
        check(address, "", Duration::from_secs(2)).await,
        "the listener still serves"
    );
    stop.send(()).expect("the server listens for the signal");
    let _drained = served.await.expect("the task ends");
}

#[tokio::test]
async fn a_tls_connection_that_sends_no_request_byte_is_closed_at_the_header_read_timeout() {
    let certificate = certificate();
    let section = ServerSection {
        tls: Some(certificate.section.clone()),
        ..short_head_bound()
    };
    let (address, stop, served) = start_run(Router::new(), section).await;
    let pinned = crate::tls::HealthcheckTls::read(&certificate.section).expect("the pin reads");
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connects");
    let mut stream = tokio_rustls::TlsConnector::from(pinned.config())
        .connect(
            rustls::pki_types::ServerName::IpAddress(address.ip().into()),
            stream,
        )
        .await
        .expect("the handshake completes");
    assert!(
        closed_after(&mut stream).await.is_some(),
        "a connection silent after its handshake is closed once the bound passes"
    );
    stop.send(()).expect("the server listens for the signal");
    let _drained = served.await.expect("the task ends");
}

#[tokio::test]
async fn a_large_body_left_unread_closes_the_connection_with_connection_close() {
    let (address, stop, served) = start_run(Router::new(), short_head_bound()).await;
    let answer = post_then_get(address, "/definition/template/t/validation", 300_000).await;
    let first = answer.split("\r\n\r\n").next().unwrap_or_default();
    assert!(first.starts_with("http/1.1 404"), "{answer}");
    let second_answered = answer.matches("http/1.1 ").count() > 1;
    assert!(
        second_answered || first.contains("\r\nconnection: close"),
        "the second request is answered or the first response says it closes: {answer}"
    );
    stop.send(()).expect("the server listens for the signal");
    let _drained = served.await.expect("the task ends");
}

#[tokio::test]
async fn a_body_read_to_its_end_keeps_the_connection() {
    let product = Router::new().route(
        "/echo",
        axum::routing::post(|body: axum::body::Bytes| async move { body.len().to_string() }),
    );
    let (address, stop, served) = start_run(product, short_head_bound()).await;
    let answer = post_then_get(address, "/echo", 300_000).await;
    let first = answer.split("\r\n\r\n").next().unwrap_or_default();
    assert!(first.starts_with("http/1.1 200"), "{answer}");
    assert!(!first.contains("connection: close"), "{answer}");
    assert_eq!(
        2,
        answer.matches("http/1.1 200").count(),
        "the connection serves the second request: {answer}"
    );
    stop.send(()).expect("the server listens for the signal");
    let _drained = served.await.expect("the task ends");
}

#[test]
fn an_accept_error_pauses_more_each_time_and_is_logged_once_a_window() {
    let mut errors = crate::serve::AcceptErrors::default();
    let now = tokio::time::Instant::now();
    let emfile = std::io::Error::from_raw_os_error(24);
    let pauses: Vec<_> = (0..10).map(|_| errors.failed(&emfile, now)).collect();
    assert_eq!(crate::serve::ACCEPT_PAUSE_FIRST, pauses[0]);
    assert_eq!(crate::serve::ACCEPT_PAUSE_FIRST * 2, pauses[1]);
    assert_eq!(crate::serve::ACCEPT_PAUSE_MOST, pauses[9], "{pauses:?}");
    assert_eq!(9, errors.suppressed, "one warning in the window");
    let later = now + crate::serve::ACCEPT_LOG_EVERY;
    let _pause = errors.failed(&emfile, later);
    assert_eq!(0, errors.suppressed, "the next window warns again");
    errors.accepted();
    assert_eq!(
        crate::serve::ACCEPT_PAUSE_FIRST,
        errors.failed(&emfile, later)
    );
    let reset = std::io::Error::from(std::io::ErrorKind::ConnectionAborted);
    assert_eq!(Duration::ZERO, errors.failed(&reset, later));
}

/// A listener whose every `accept` fails with `EMFILE`, counting the
/// attempts; after `limit` of them it never answers again.
struct Exhausted {
    attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    limit: usize,
}

impl crate::serve::Accept for Exhausted {
    async fn accept(&mut self) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
        let attempts = self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if attempts >= self.limit {
            std::future::pending::<()>().await;
        }
        Err(std::io::Error::from_raw_os_error(24))
    }
}

#[tokio::test(start_paused = true)]
async fn the_accept_loop_backs_off_while_descriptors_are_exhausted() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = Exhausted {
        attempts: attempts.clone(),
        limit: 10_000,
    };
    let connections =
        crate::serve::Connections::new(Router::new(), None, crate::serve::DEFAULT_BOUNDS);
    let drained = crate::serve::drive(
        listener,
        connections,
        Lifecycle::default(),
        tokio::time::sleep(Duration::from_secs(3)),
        Duration::ZERO,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(Drained::Complete, drained);
    let attempts = attempts.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        (2..=12).contains(&attempts),
        "three seconds of EMFILE take a few attempts, not a spin: {attempts}"
    );
}
