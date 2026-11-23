// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What a refusal past the admission bound costs the server, and that the
//! refusals stay visible: the counter by reason and one log line a second.
//!
//! The cost is the CPU time of the server's own thread over thousands of
//! refusals: the stack runs on a current-thread runtime of its own behind a
//! loopback listener, with the request log and the admission metered and
//! every log line formatted as JSON and written to the null device, and the
//! client sends from another thread over one kept-alive connection. CPU
//! time is what the refusals take from the requests the server serves, and
//! unlike the wall clock it does not grow when other tests share the cores.
//!
//! The same thread serves a bare axum route that answers `503` at once, the
//! floor of an exchange on this transport. In every build a refusal costs
//! at most [`FLOOR_RATIO`] times the floor; an optimized build also holds
//! it to [`CEILING`], since unoptimized hyper and axum alone take about as
//! much (42 us against 27 us for the floor on an Apple M-series host).

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use ferrohealth_telemetry::request_log::RequestLog;
use ferrohealth_telemetry::section::TelemetrySection;
use ferrohealth_telemetry::telemetry::{Service, Telemetry};
use http::StatusCode;
use http_body_util::BodyExt as _;
use hyper_util::rt::TokioIo;
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};

use ferrohealth_server::section::{BasePath, ServerSection};
use ferrohealth_server::stack::{StackPolicy, app};

use super::lines::capturing;
use super::tracing_subscriber_shim::Capture;
use super::{get_status, serving};

/// The refusals measured in each of [`ROUNDS`], after [`WARMUP`] that are
/// not.
const REFUSALS: u32 = 1_000;

/// The rounds, each measuring the floor and then the refusals, so a drift
/// of the host's speed reaches both alike.
const ROUNDS: u32 = 4;

/// The refusals sent before the measure, so first-use costs stay out of it.
const WARMUP: u32 = 500;

/// The most server CPU one refusal may take in an optimized build.
const CEILING: Duration = Duration::from_micros(50);

/// The most a refusal may cost against the floor, in every build: the old
/// path, which logged twice and ran every layer first, cost 3.3 times it.
const FLOOR_RATIO: u32 = 2;

/// The CPU time the calling thread has used.
fn thread_cpu() -> Duration {
    let spent = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Duration::new(
        u64::try_from(spent.tv_sec).expect("a CPU time is never negative"),
        u32::try_from(spent.tv_nsec).expect("nanoseconds below a second"),
    )
}

/// A product whose `/held` takes the one slot until `release` opens, and
/// says so on `entered`.
fn held(release: Arc<Semaphore>, entered: Arc<Notify>) -> Router {
    Router::new()
        .route(
            "/held",
            get(move || {
                let (release, entered) = (Arc::clone(&release), Arc::clone(&entered));
                async move {
                    entered.notify_one();
                    let _permit = release.acquire().await.expect("the gate stays open");
                    "held"
                }
            }),
        )
        .route("/fhir/Patient/{id}", get(|| async { "served" }))
}

/// The server: the stack over [`held`] with one slot and no queue, on a
/// current-thread runtime of its own thread, answering each sender on its
/// channel with that thread's CPU time.
struct Server {
    address: std::net::SocketAddr,
    floor: std::net::SocketAddr,
    release: Arc<Semaphore>,
    entered: Arc<Notify>,
    cpu: mpsc::UnboundedSender<oneshot::Sender<Duration>>,
    thread: std::thread::JoinHandle<()>,
}

impl Server {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let address = listener.local_addr().expect("the bound address");
        let bare = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        bare.set_nonblocking(true).expect("a non-blocking listener");
        let floor = bare.local_addr().expect("the bound address");
        let release = Arc::new(Semaphore::new(0));
        let entered = Arc::new(Notify::new());
        let (cpu, mut asks) = mpsc::unbounded_channel::<oneshot::Sender<Duration>>();
        let product = held(Arc::clone(&release), Arc::clone(&entered));
        let thread = std::thread::spawn(move || {
            let null = std::fs::File::create("/dev/null").expect("the null device");
            let subscriber = tracing_subscriber::fmt()
                .json()
                .flatten_event(true)
                .with_writer(std::sync::Mutex::new(null))
                .finish();
            let _default = tracing::subscriber::set_default(subscriber);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the server runtime");
            runtime.block_on(async move {
                let telemetry = Telemetry::new(
                    &TelemetrySection::default(),
                    Service::new("ferrosys", "0.0.0"),
                )
                .expect("the telemetry builds");
                let meter = telemetry.meter("ferrohealth_server");
                let section = ServerSection {
                    max_concurrent_requests: NonZeroUsize::MIN,
                    max_waiting_requests: 0,
                    ..ServerSection::default()
                };
                let policy = StackPolicy {
                    request_log: RequestLog::default().with_metrics(&meter),
                    meter: Some(meter),
                    ..StackPolicy::default()
                };
                let app = app(product, serving(), &section, policy);
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("a tokio listener");
                let served = tokio::spawn(async move {
                    axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .await
                    .expect("the server serves");
                });
                let bare = tokio::net::TcpListener::from_std(bare).expect("a tokio listener");
                let busy = Router::new().route(
                    "/fhir/Patient/{id}",
                    get(|| async {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            [(http::header::RETRY_AFTER, "1")],
                            "the server is busy; retry later",
                        )
                    }),
                );
                let floored = tokio::spawn(async move {
                    axum::serve(bare, busy).await.expect("the floor serves");
                });
                while let Some(ask) = asks.recv().await {
                    ask.send(thread_cpu()).expect("the asker waits");
                }
                served.abort();
                floored.abort();
            });
        });
        Self {
            address,
            floor,
            release,
            entered,
            cpu,
            thread,
        }
    }

    async fn cpu(&self) -> Duration {
        let (answer, read) = oneshot::channel();
        self.cpu.send(answer).expect("the server runs");
        read.await.expect("the server answers")
    }

    fn stop(self) {
        drop(self.cpu);
        self.thread.join().expect("the server thread ends");
    }
}

/// One kept-alive HTTP/1.1 connection to `server`.
async fn connect(
    address: std::net::SocketAddr,
) -> hyper::client::conn::http1::SendRequest<http_body_util::Empty<axum::body::Bytes>> {
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("the server accepts");
    let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .expect("the handshake");
    tokio::spawn(connection);
    sender
}

/// Sends `GET uri` on `sender` and returns the status and the body.
async fn send(
    sender: &mut hyper::client::conn::http1::SendRequest<http_body_util::Empty<axum::body::Bytes>>,
    uri: &str,
) -> (StatusCode, http::HeaderMap) {
    sender.ready().await.expect("the connection is ready");
    let response = sender
        .send_request(
            http::Request::get(uri)
                .header(http::header::HOST, "localhost")
                .body(http_body_util::Empty::new())
                .expect("the request builds"),
        )
        .await
        .expect("the server answers");
    let (parts, body) = response.into_parts();
    body.collect().await.expect("the body");
    (parts.status, parts.headers)
}

/// The server CPU `count` requests to `uri` on `sender` take, each
/// answered `503` with `Retry-After`.
async fn cpu_of(
    server: &Server,
    sender: &mut hyper::client::conn::http1::SendRequest<http_body_util::Empty<axum::body::Bytes>>,
    uri: &str,
    count: u32,
) -> Duration {
    let before = server.cpu().await;
    for _ in 0..count {
        let (status, headers) = send(sender, uri).await;
        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
        assert!(headers.contains_key(http::header::RETRY_AFTER));
    }
    server.cpu().await.saturating_sub(before)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_refusal_costs_the_server_at_most_fifty_microseconds_of_cpu() {
    let server = Server::start();
    let mut holder = connect(server.address).await;
    let entered = Arc::clone(&server.entered);
    let entered = entered.notified();
    let hold = tokio::spawn(async move { send(&mut holder, "/held").await });
    entered.await;
    let mut client = connect(server.address).await;
    let mut bare = connect(server.floor).await;
    cpu_of(&server, &mut client, "/fhir/Patient/7", WARMUP).await;
    cpu_of(&server, &mut bare, "/fhir/Patient/7", WARMUP).await;
    let (mut floor, mut refused) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..ROUNDS {
        floor += cpu_of(&server, &mut bare, "/fhir/Patient/7", REFUSALS).await;
        refused += cpu_of(&server, &mut client, "/fhir/Patient/7", REFUSALS).await;
    }
    server.release.add_permits(1);
    let (status, _) = hold.await.expect("the held request ends");
    assert_eq!(StatusCode::OK, status);
    let (status, _) = send(&mut client, "/fhir/Patient/7").await;
    assert_eq!(StatusCode::OK, status, "a free slot serves again");
    drop((client, bare));
    server.stop();
    let (floor, each) = (floor / (ROUNDS * REFUSALS), refused / (ROUNDS * REFUSALS));
    assert!(
        each <= floor * FLOOR_RATIO,
        "a refusal took {each:?} of server CPU, more than {FLOOR_RATIO} times the {floor:?} of a bare 503"
    );
    if cfg!(not(debug_assertions)) {
        assert!(
            each <= CEILING,
            "a refusal took {each:?} of server CPU, more than {CEILING:?} (a bare 503 took {floor:?})"
        );
    }
}

/// The lines `capture` holds whose message is `message`.
fn lines_of(capture: &Capture, message: &str) -> Vec<serde_json::Value> {
    let log = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
    log.lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .filter(|line: &serde_json::Value| {
            line["message"]
                .as_str()
                .is_some_and(|text| text.starts_with(message))
        })
        .collect()
}

/// The counts of the refusal lines in `capture`, full queue and expired
/// wait.
fn refusal_counts(capture: &Capture) -> Vec<(Option<u64>, Option<u64>)> {
    lines_of(capture, "requests were refused")
        .iter()
        .map(|line| (line["queue_full"].as_u64(), line["wait_expired"].as_u64()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn refusals_are_one_line_a_second_with_their_count_and_no_request_line() {
    let (capture, _default) = capturing();
    let release = Arc::new(Semaphore::new(0));
    let entered = Arc::new(Notify::new());
    let section = ServerSection {
        base_path: BasePath::new("/ferrosys").expect("a base path"),
        max_concurrent_requests: NonZeroUsize::MIN,
        max_waiting_requests: 0,
        request_timeout: ferrohealth_config::units::Duration::from_secs(60),
        ..ServerSection::default()
    };
    let app = app(
        held(Arc::clone(&release), Arc::clone(&entered)),
        serving(),
        &section,
        StackPolicy::default(),
    );
    let notified = entered.notified();
    let hold = tokio::spawn(get_status(app.clone(), "/ferrosys/held"));
    notified.await;
    for _ in 0..100 {
        let (status, headers, _) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
        assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
        assert_eq!(
            36,
            headers
                .get("x-request-id")
                .map_or(0, http::HeaderValue::len),
            "the refusal echoes a server id"
        );
    }
    assert_eq!(
        vec![(Some(1), Some(0))],
        refusal_counts(&capture),
        "the first refusal is written at once, the 99 after it wait for the next line"
    );
    tokio::time::sleep(Duration::from_millis(999)).await;
    let (status, _, _) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(
        1,
        refusal_counts(&capture).len(),
        "within the second, no line"
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    let (status, _, _) = get_status(app.clone(), "/ferrosys/fhir/Patient/7").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(
        vec![(Some(1), Some(0)), (Some(101), Some(0))],
        refusal_counts(&capture),
        "a second on, one line counts every refusal since the first"
    );
    let (status, _, _) = get_status(app.clone(), "/ferrosys/health/readiness").await;
    assert_eq!(StatusCode::OK, status, "the health family passes the bound");
    release.add_permits(1);
    let (status, _, _) = hold.await.expect("the held request ends");
    assert_eq!(StatusCode::OK, status);
    let statuses: Vec<_> = lines_of(&capture, "request")
        .iter()
        .filter(|line| line["message"] == "request")
        .map(|line| {
            (
                line["route"].as_str().map(str::to_owned),
                line["status"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        vec![
            (Some("/ferrosys/health/readiness".to_owned()), Some(200)),
            (Some("/ferrosys/held".to_owned()), Some(200)),
        ],
        statuses,
        "the refusals leave no request line, the requests served leave theirs"
    );
}
