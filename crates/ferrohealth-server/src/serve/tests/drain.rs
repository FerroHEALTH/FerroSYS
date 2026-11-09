// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The drain: the signal, the bound on requests and connections, every
//! listener on one signal, and an HTTP/2 handler cut at the bound.

use std::time::Duration;

use axum::Router;
use axum::routing::get;
use ferrohealth_health::document::Phase;
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use tokio::net::TcpListener;

use crate::health::Health;
use crate::healthcheck::{Outcome, check};
use crate::section::ServerSection;
use crate::serve::Drained;
use crate::stack::{StackPolicy, app};

use super::{fetch, start};

#[tokio::test]
async fn the_signal_withdraws_readiness_then_the_drain_completes() {
    let (address, lifecycle, stop, served) = start(
        Router::new(),
        Duration::from_millis(400),
        Duration::from_secs(5),
    )
    .await;
    let healthy = check(address, "", Duration::from_secs(2)).await;
    assert_eq!(Outcome::Healthy, healthy);
    assert_eq!(Phase::Serving, lifecycle.phase());
    stop.send(()).expect("the server listens for the signal");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(Phase::Draining, lifecycle.phase());
    let during = check(address, "", Duration::from_secs(2)).await;
    assert_eq!(
        Outcome::Unhealthy(503),
        during,
        "readiness is withdrawn during the delay"
    );
    let drained = served
        .await
        .expect("the task ends")
        .expect("the server serves");
    assert_eq!(Drained::Complete, drained);
}

#[tokio::test]
async fn a_request_still_running_past_the_bound_is_cut() {
    let product = Router::new().route(
        "/hold",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            "held"
        }),
    );
    let (address, _lifecycle, stop, served) =
        start(product, Duration::ZERO, Duration::from_millis(300)).await;
    let held = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("a connection");
        stream
            .write_all(b"GET /hold HTTP/1.1\r\nhost: localhost\r\n\r\n")
            .await
            .expect("the request is sent");
        let mut buffer = Vec::new();
        let _read = stream.read_to_end(&mut buffer).await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(()).expect("the server listens for the signal");
    let drained = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the drain is bounded")
        .expect("the task ends")
        .expect("the server serves");
    assert_eq!(Drained::Cut, drained);
    held.abort();
}

#[tokio::test]
async fn a_connection_still_open_at_the_bound_is_closed_when_run_returns() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let product = Router::new().route(
        "/hold",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            "held"
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let section = ServerSection {
        drain_delay: ferrohealth_config::units::Duration::ZERO,
        shutdown_timeout: ferrohealth_config::units::Duration::from_millis(300),
        ..ServerSection::default()
    };
    let app = app(
        product,
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
        .expect("a connection");
    stream
        .write_all(b"GET /hold HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .expect("the request is sent");
    tokio::time::sleep(Duration::from_millis(100)).await;
    stop.send(()).expect("the server listens for the signal");
    let drained = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the drain is bounded")
        .expect("the task ends")
        .expect("the server serves");
    assert_eq!(Drained::Cut, drained);
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut rest)).await;
    assert!(
        read.is_ok(),
        "the held connection is closed once run returns"
    );
    assert!(rest.is_empty(), "the held request got no answer");
}

#[tokio::test]
async fn every_listener_drains_on_one_signal() {
    let lifecycle = Lifecycle::default();
    let health = Health::new(Registry::default(), lifecycle.clone());
    let public = Router::new().route(
        "/hold",
        get(|| async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            "held"
        }),
    );
    let section = ServerSection {
        shutdown_timeout: ferrohealth_config::units::Duration::from_millis(5_000),
        ..ServerSection::default()
    };
    let mut listeners = Vec::new();
    let mut addresses = Vec::new();
    for product in [
        public,
        Router::new().route("/metrics", get(|| async { "up 1" })),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        addresses.push(listener.local_addr().expect("the address"));
        let app = app(product, health.clone(), &section, StackPolicy::default());
        listeners.push(crate::serve::Listener::new(listener, app));
    }
    let [public, admin] = addresses[..] else {
        panic!("two listeners")
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn({
        let lifecycle = lifecycle.clone();
        async move {
            crate::serve::run_all(listeners, &section, lifecycle, async move {
                let _received = stopped.await;
            })
            .await
        }
    });
    assert!(fetch(admin, "/metrics").await.ends_with("up 1"));
    let held = tokio::spawn(fetch(public, "/hold"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.send(()).expect("the server listens");
    assert!(
        held.await.expect("the request").ends_with("held"),
        "the request in flight finishes"
    );
    let drained = served.await.expect("the task").expect("the drain");
    assert_eq!(Drained::Complete, drained);
    assert_eq!(Phase::Draining, lifecycle.phase());
    for address in [public, admin] {
        assert!(
            tokio::net::TcpStream::connect(address).await.is_err(),
            "{address} is closed"
        );
    }
}

/// A handler that never answers and, once woken, holds its worker for
/// half a second in each poll, so only an abort the drain waits for
/// ends it before `run` returns.
struct Stubborn {
    polls: usize,
    started: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    _running: Running,
}

impl Future for Stubborn {
    type Output = &'static str;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polls += 1;
        if self.polls == 1 {
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        } else {
            std::thread::sleep(Duration::from_millis(500));
        }
        std::task::Poll::Pending
    }
}

/// Counts its own drop, so a test sees a handler cancelled.
struct Running(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_http2_handler_still_running_at_the_bound_is_cancelled_when_run_returns() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let product = Router::new().route(
        "/hold",
        get({
            let (started, dropped) = (started.clone(), dropped.clone());
            move || Stubborn {
                polls: 0,
                started,
                _running: Running(dropped),
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let section = ServerSection {
        drain_delay: ferrohealth_config::units::Duration::ZERO,
        shutdown_timeout: ferrohealth_config::units::Duration::from_millis(300),
        ..ServerSection::default()
    };
    let app = app(
        product,
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
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("a connection");
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .expect("the HTTP/2 handshake");
    let client = tokio::spawn(connection);
    let request = tokio::spawn(
        sender.send_request(
            http::Request::get("http://localhost/hold")
                .body(axum::body::Body::empty())
                .expect("the request builds"),
        ),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while started.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the handler starts");
    stop.send(()).expect("the server listens for the signal");
    let drained = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .expect("the drain is bounded")
        .expect("the task ends")
        .expect("the server serves");
    assert_eq!(Drained::Cut, drained);
    assert_eq!(
        1,
        dropped.load(Ordering::SeqCst),
        "the handler is cancelled before run returns"
    );
    request.abort();
    client.abort();
}
