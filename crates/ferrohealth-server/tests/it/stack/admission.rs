// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The queue in front of the overload bound: a request past the bound
//! waits for a slot, first come first served, until the queue is full or
//! the wait expires. Every test runs on paused Tokio time over handlers the
//! test releases, so no outcome hangs on the wall clock.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::routing::get;
use ferrohealth_config::units::Duration as ConfigDuration;
use ferrohealth_telemetry::section::TelemetrySection;
use ferrohealth_telemetry::telemetry::{Service, Telemetry};
use http::StatusCode;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{StackPolicy, app};

use super::{get_status, serving};

type Answer = (StatusCode, http::HeaderMap, String);

/// A product whose route `/gated/{id}` notes `id` in the order the requests
/// reach it, then holds each until the test adds a permit to its gate.
struct Gated {
    gate: Arc<Semaphore>,
    order: Arc<Mutex<Vec<usize>>>,
    held: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Gated {
    fn new() -> Self {
        Self {
            gate: Arc::new(Semaphore::new(0)),
            order: Arc::new(Mutex::new(Vec::new())),
            held: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn router(&self) -> Router {
        let (gate, order, held, peak) = (
            Arc::clone(&self.gate),
            Arc::clone(&self.order),
            Arc::clone(&self.held),
            Arc::clone(&self.peak),
        );
        Router::new().route(
            "/gated/{id}",
            get(move |Path(id): Path<usize>| {
                let (gate, order, held, peak) =
                    (gate.clone(), order.clone(), held.clone(), peak.clone());
                async move {
                    order.lock().expect("the order").push(id);
                    let now = held.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    gate.acquire().await.expect("the gate stays open").forget();
                    held.fetch_sub(1, Ordering::SeqCst);
                    "served"
                }
            }),
        )
    }

    fn release(&self, requests: usize) {
        self.gate.add_permits(requests);
    }

    fn held(&self) -> usize {
        self.held.load(Ordering::SeqCst)
    }

    fn order(&self) -> Vec<usize> {
        self.order.lock().expect("the order").clone()
    }
}

/// The section of `limit` slots and `places` places, waiting 100 ms.
fn bounded(limit: usize, places: usize) -> ServerSection {
    ServerSection {
        max_concurrent_requests: NonZeroUsize::new(limit).expect("a bound"),
        max_waiting_requests: places,
        admission_wait: ConfigDuration::from_millis(100),
        retry_after: ConfigDuration::from_secs(2),
        ..ServerSection::default()
    }
}

/// Sends `GET /gated/{id}` and lets it run until it blocks, in the slot or
/// in the queue: paused time moves the 1 ms only once every task is idle.
async fn spawn(app: &Router, id: usize) -> JoinHandle<Answer> {
    let request = tokio::spawn(request(app.clone(), id));
    settle().await;
    request
}

/// Sends `GET /gated/{id}`.
async fn request(app: Router, id: usize) -> Answer {
    get_status(app, &format!("/gated/{id}")).await
}

/// Returns once every spawned task has run until it blocks.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(1)).await;
}

fn retry_after(headers: &http::HeaderMap) -> Option<&str> {
    headers.get("retry-after").and_then(|v| v.to_str().ok())
}

#[tokio::test(start_paused = true)]
async fn past_the_bound_a_request_that_gets_a_slot_within_the_wait_is_served() {
    let gated = Gated::new();
    let app = app(
        gated.router(),
        serving(),
        &bounded(1, 4),
        StackPolicy::default(),
    );
    let first = spawn(&app, 0).await;
    let second = spawn(&app, 1).await;
    assert_eq!(
        1,
        gated.held(),
        "the second request waits outside the handler"
    );
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!second.is_finished(), "it is still waiting, unrefused");
    gated.release(2);
    let (status, _, body) = second.await.expect("the request ends");
    assert_eq!((StatusCode::OK, "served"), (status, body.as_str()));
    let (status, _, _) = first.await.expect("the request ends");
    assert_eq!(StatusCode::OK, status);
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_refuses_at_once_and_readiness_is_answered() {
    let gated = Gated::new();
    let app = app(
        gated.router(),
        serving(),
        &bounded(1, 1),
        StackPolicy::default(),
    );
    let first = spawn(&app, 0).await;
    let waiting = spawn(&app, 1).await;
    let started = Instant::now();
    let (status, headers, _) = get_status(app.clone(), "/gated/2").await;
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(Some("2"), retry_after(&headers));
    let (status, _, _) = get_status(app.clone(), "/health/readiness").await;
    assert_eq!(StatusCode::OK, status, "readiness is never queued");
    assert_eq!(
        Duration::ZERO,
        started.elapsed(),
        "the refusal and readiness come at once"
    );
    assert!(!waiting.is_finished(), "the queued request still waits");
    gated.release(2);
    for request in [first, waiting] {
        let (status, _, _) = request.await.expect("the request ends");
        assert_eq!(StatusCode::OK, status);
    }
}

#[tokio::test(start_paused = true)]
async fn no_queue_refuses_at_once() {
    let gated = Gated::new();
    let app = app(
        gated.router(),
        serving(),
        &bounded(1, 0),
        StackPolicy::default(),
    );
    let first = spawn(&app, 0).await;
    let started = Instant::now();
    let (status, headers, _) = get_status(app, "/gated/1").await;
    assert_eq!(
        (StatusCode::SERVICE_UNAVAILABLE, Some("2"), Duration::ZERO),
        (status, retry_after(&headers), started.elapsed())
    );
    gated.release(1);
    let (status, _, _) = first.await.expect("the request ends");
    assert_eq!(StatusCode::OK, status);
}

#[tokio::test(start_paused = true)]
async fn an_expired_wait_is_refused_with_retry_after() {
    let gated = Gated::new();
    let app = app(
        gated.router(),
        serving(),
        &bounded(1, 4),
        StackPolicy::default(),
    );
    let first = spawn(&app, 0).await;
    let started = Instant::now();
    let (status, headers, _) = get_status(app.clone(), "/gated/1").await;
    let waited = started.elapsed();
    assert_eq!(StatusCode::SERVICE_UNAVAILABLE, status);
    assert_eq!(Some("2"), retry_after(&headers));
    assert!(
        waited >= Duration::from_millis(100) && waited < Duration::from_millis(110),
        "refused when the wait of 100 ms expired, after {waited:?}"
    );
    gated.release(1);
    let (status, _, _) = first.await.expect("the request ends");
    assert_eq!(StatusCode::OK, status);
}

#[tokio::test(start_paused = true)]
async fn waiting_requests_are_admitted_first_come_first_served() {
    let gated = Gated::new();
    let app = app(
        gated.router(),
        serving(),
        &bounded(1, 8),
        StackPolicy::default(),
    );
    let mut requests = Vec::new();
    for id in 0..6 {
        requests.push(spawn(&app, id).await);
    }
    assert_eq!(vec![0], gated.order(), "one in the slot, five waiting");
    gated.release(6);
    for request in requests {
        let (status, _, _) = request.await.expect("the request ends");
        assert_eq!(StatusCode::OK, status);
    }
    assert_eq!(vec![0, 1, 2, 3, 4, 5], gated.order());
}

#[tokio::test(start_paused = true)]
async fn under_a_burst_the_requests_held_never_exceed_the_two_bounds() {
    const OFFERED: usize = 1_000;
    let gated = Gated::new();
    let section = ServerSection::default();
    let (limit, places) = (
        section.max_concurrent_requests.get(),
        section.max_waiting_requests,
    );
    assert_eq!((256, 256), (limit, places), "the defaults");
    let app = app(gated.router(), serving(), &section, StackPolicy::default());
    let requests: Vec<_> = (0..OFFERED)
        .map(|id| tokio::spawn(request(app.clone(), id)))
        .collect();
    settle().await;
    let finished = requests.iter().filter(|r| r.is_finished()).count();
    assert_eq!(limit, gated.held(), "every slot is taken");
    assert_eq!(
        OFFERED - limit - places,
        finished,
        "past the slots and the queue every request is refused at once"
    );
    let (status, _, _) = get_status(app.clone(), "/health/readiness").await;
    assert_eq!(StatusCode::OK, status, "readiness is answered while full");
    gated.release(OFFERED);
    let (mut served, mut refused) = (0, 0);
    for request in requests {
        match request.await.expect("the request ends").0 {
            StatusCode::OK => served += 1,
            StatusCode::SERVICE_UNAVAILABLE => refused += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!(limit, gated.peak.load(Ordering::SeqCst), "never more held");
    assert_eq!(
        (limit + places, OFFERED - limit - places),
        (served, refused),
        "the waiters are served once slots come free"
    );
}

#[tokio::test(start_paused = true)]
async fn the_waits_and_the_refusals_by_reason_reach_the_meter() {
    let telemetry = Telemetry::new(
        &TelemetrySection::default(),
        Service::new("ferrosys", "0.0.0"),
    )
    .expect("the telemetry builds");
    let policy = StackPolicy {
        meter: Some(telemetry.meter("ferrohealth_server")),
        ..StackPolicy::default()
    };
    let gated = Gated::new();
    let app = app(gated.router(), serving(), &bounded(1, 1), policy);
    let first = spawn(&app, 0).await;
    let admitted = spawn(&app, 1).await;
    let (full, _, _) = get_status(app.clone(), "/gated/2").await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    gated.release(2);
    for request in [first, admitted] {
        assert_eq!(StatusCode::OK, request.await.expect("the request ends").0);
    }
    let blocker = spawn(&app, 3).await;
    let (expired, _, _) = get_status(app.clone(), "/gated/4").await;
    gated.release(1);
    assert_eq!(StatusCode::OK, blocker.await.expect("the request ends").0);
    assert_eq!(
        (
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::SERVICE_UNAVAILABLE
        ),
        (full, expired)
    );
    let text = telemetry.render_prometheus().expect("the metrics render");
    let value = |series: &str| {
        text.lines()
            .find(|line| line.starts_with(series))
            .and_then(|line| line.rsplit(' ').next())
            .map(str::to_owned)
    };
    assert_eq!(
        Some("2".to_owned()),
        value("ferrohealth_server_admission_waits_total"),
        "{text}"
    );
    for (reason, count) in [("queue_full", "1"), ("wait_expired", "1")] {
        let series = format!(
            "ferrohealth_server_admission_refusals_total{{ferrohealth_server_admission_reason=\"{reason}\""
        );
        assert_eq!(Some(count.to_owned()), value(&series), "{text}");
    }
    for outcome in ["admitted", "expired"] {
        let series = format!(
            "ferrohealth_server_admission_wait_duration_seconds_count{{ferrohealth_server_admission_outcome=\"{outcome}\""
        );
        assert_eq!(Some("1".to_owned()), value(&series), "{text}");
    }
    let admitted_sum = "ferrohealth_server_admission_wait_duration_seconds_sum{ferrohealth_server_admission_outcome=\"admitted\"";
    assert_eq!(
        Some("0.031".to_owned()),
        value(admitted_sum),
        "the 1 ms of its spawn and the 30 ms after: {text}"
    );
}
