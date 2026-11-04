// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the reporter: the reports it sends, what it does on a
//! refusal or an unreachable collector, one store shared by two processes,
//! the switch and the host memory.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::{Method, StatusCode};
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    Identity, Notice, Off, ProductSection, ReportError, Reporter, StartOutcome, announcement,
    meminfo_total_bytes, prepare,
};
use crate::envelope::{Key, LicenceGrant, Product};
use crate::section::{Endpoint, UsageReportSection};
use crate::state::{DailyClaim, Instance, Store, StoreFuture};
use crate::window::Counts;
use crate::window::Window;

#[derive(Debug, serde::Serialize)]
struct Status {
    lanes: Vec<&'static str>,
    auth: bool,
    domains: u64,
}

#[derive(Debug)]
struct Pix;

impl ProductSection for Pix {
    type Status = Status;
    const PRODUCT: Product = Product::Pix;

    fn status(&self) -> impl Future<Output = Status> + Send {
        std::future::ready(Status {
            lanes: vec!["pdqm", "pixm"],
            auth: true,
            domains: 3,
        })
    }
}

const NOTICE: Notice = Notice {
    contents: "instance id, version and coarse latency",
    switch: "FERROSYS__USAGE_REPORT__ENABLED=false",
    state_dir_variable: "FERROSYS__USAGE_REPORT__STATE_DIR",
    url: "https://ferrohealth.eu/docs/usage-report.html",
};

fn grouping(method: &Method, route: Option<&str>) -> Option<Key> {
    match (method, route) {
        (_, Some(route)) if route.starts_with("/health") => None,
        (&Method::GET, Some(_)) => Key::new("pdqm_search"),
        _ => Key::new("other"),
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/specs/ferropulse/schemas")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("the vendored file reads"))
        .expect("JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&read_json(&fixtures().join("report-envelope.v2.json")))
        .expect("the schema compiles")
}

fn examples(validity: &str) -> Vec<(String, Value)> {
    let dir = fixtures()
        .join("examples/report-envelope.v2")
        .join(validity);
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .expect("the examples are vendored")
        .map(|entry| {
            let path = entry.expect("an entry").path();
            (
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                read_json(&path),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn assert_valid(report: &Value) {
    let errors: Vec<String> = validator()
        .iter_errors(report)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{report:#}");
}

fn reporter(collector: &str, state_dir: &Path) -> Reporter<Pix> {
    let section = UsageReportSection {
        endpoint: Endpoint::new(collector).expect("a loopback collector"),
        state_dir: Some(state_dir.to_path_buf()),
        ..UsageReportSection::default()
    };
    let prepared = prepare(&section, None).expect("the report runs");
    let window = Arc::new(Window::new(grouping));
    window.record(
        &Method::GET,
        Some("/fhir/Patient"),
        StatusCode::OK,
        Duration::from_millis(12),
    );
    window.record(
        &Method::POST,
        None,
        StatusCode::BAD_REQUEST,
        Duration::from_millis(2),
    );
    window.record(
        &Method::GET,
        Some("/health/readiness"),
        StatusCode::OK,
        Duration::from_millis(1),
    );
    Reporter::new(
        &section,
        Identity {
            version: "0.1.0".to_owned(),
            git_sha: "1a2b3c4".to_owned(),
            licence: LicenceGrant::NonCommercial,
        },
        prepared,
        Arc::new(Pix),
        window,
        Instant::now(),
    )
    .expect("the client builds")
}

#[test]
fn the_validator_takes_every_valid_example_and_refuses_every_invalid_one() {
    let validator = validator();
    let valid = examples("valid");
    assert!(!valid.is_empty(), "the valid examples are vendored");
    for (name, example) in valid {
        assert!(validator.is_valid(&example), "{name} is valid");
    }
    let invalid = examples("invalid");
    assert!(!invalid.is_empty(), "the invalid examples are vendored");
    for (name, example) in invalid {
        assert!(!validator.is_valid(&example), "{name} is refused");
    }
}

#[tokio::test]
async fn the_start_and_daily_reports_validate_against_the_envelope() {
    let dir = tempfile::tempdir().expect("a state directory");
    let reporter = reporter("http://127.0.0.1:9/v1/report", dir.path());
    let start =
        serde_json::to_value(reporter.start_report().await).expect("the start report serializes");
    assert_valid(&start);
    assert_eq!("start", start["event"]);
    assert_eq!(true, start["first_start"], "the first start created the id");
    assert_eq!("ferropix", start["product"]);
    let daily =
        serde_json::to_value(reporter.daily_report().await).expect("the daily report serializes");
    assert_valid(&daily);
    let window = &daily["metrics"]["window_24h"];
    assert_eq!("1-100", window["requests_bucket"]);
    assert_eq!(1, window["counters"]["responses_4xx"]);
    assert!(
        window["groups"]["pdqm_search"]["histogram"].is_array(),
        "{daily:#}"
    );
    assert!(
        window["groups"].get("health").is_none(),
        "a probe is not counted"
    );
}

#[tokio::test]
async fn a_report_reaches_the_collector_and_a_refusal_keeps_its_status() {
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/report"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(202))
        .up_to_n_times(1)
        .mount(&collector)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/report"))
        .respond_with(ResponseTemplate::new(422))
        .mount(&collector)
        .await;
    let dir = tempfile::tempdir().expect("a state directory");
    let reporter = reporter(&format!("{}/v1/report", collector.uri()), dir.path());
    assert_eq!(
        StartOutcome::Sent(StatusCode::ACCEPTED),
        reporter.send_start().await.expect("the start is sent")
    );
    assert_eq!(
        StartOutcome::RateLimited,
        reporter
            .send_start()
            .await
            .expect("the second start is skipped")
    );
    assert_eq!(
        StatusCode::UNPROCESSABLE_ENTITY,
        reporter
            .send_daily()
            .await
            .expect("the daily report is sent")
    );
    let received = collector
        .received_requests()
        .await
        .expect("the requests are recorded");
    assert_eq!(2, received.len());
    let start: Value =
        serde_json::from_slice(&received.first().expect("the start").body).expect("JSON");
    assert_valid(&start);
}

#[tokio::test]
async fn an_unreachable_collector_puts_the_counts_back() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
    let address = listener.local_addr().expect("the address");
    drop(listener);
    let dir = tempfile::tempdir().expect("a state directory");
    let reporter = reporter(&format!("http://{address}/v1/report"), dir.path());
    assert!(reporter.send_daily().await.is_err(), "nothing listens");
    assert_eq!(
        2,
        reporter.window.take().requests(),
        "the counts are kept for the next try"
    );
}

/// A store two processes share, as a database would be.
#[derive(Debug)]
struct Shared {
    id: uuid::Uuid,
    started: std::sync::Mutex<bool>,
    sent_today: std::sync::Mutex<bool>,
    window: Window,
}

impl Shared {
    fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            started: std::sync::Mutex::new(false),
            sent_today: std::sync::Mutex::new(false),
            window: Window::new(grouping),
        }
    }

    fn next_day(&self) {
        *self.sent_today.lock().expect("the lock") = false;
    }
}

impl Store for Shared {
    fn instance(&self) -> StoreFuture<'_, Instance> {
        Box::pin(std::future::ready(Ok(Instance {
            id: self.id,
            created: false,
        })))
    }

    fn claim_start(&self, _now: jiff::Timestamp) -> StoreFuture<'_, bool> {
        let mut started = self.started.lock().expect("the lock");
        let claimed = !*started;
        *started = true;
        Box::pin(std::future::ready(Ok(claimed)))
    }

    fn claim_daily(&self, _now: jiff::Timestamp, local: Counts) -> StoreFuture<'_, DailyClaim> {
        self.window.restore(&local);
        let mut sent = self.sent_today.lock().expect("the lock");
        let claim = if *sent {
            DailyClaim::Sent
        } else {
            *sent = true;
            DailyClaim::Taken(self.window.take())
        };
        Box::pin(std::future::ready(Ok(claim)))
    }

    fn restore(&self, counts: Counts) -> StoreFuture<'_, Option<Counts>> {
        self.window.restore(&counts);
        Box::pin(std::future::ready(Ok(None)))
    }
}

fn shared_reporter(collector: &str, store: &Arc<Shared>, requests: u64) -> Reporter<Pix> {
    let section = UsageReportSection {
        endpoint: Endpoint::new(collector).expect("a loopback collector"),
        ..UsageReportSection::default()
    };
    let window = Arc::new(Window::new(grouping));
    for _ in 0..requests {
        window.record(
            &Method::GET,
            Some("/fhir/Patient"),
            StatusCode::OK,
            Duration::from_millis(12),
        );
    }
    let instance = Instance {
        id: store.id,
        created: false,
    };
    let store: Arc<dyn Store> = Arc::<Shared>::clone(store);
    Reporter::with_store(
        &section,
        Identity {
            version: "0.1.0".to_owned(),
            git_sha: "1a2b3c4".to_owned(),
            licence: LicenceGrant::NonCommercial,
        },
        (instance, store),
        Arc::new(Pix),
        window,
        Instant::now(),
    )
    .expect("the client builds")
}

#[tokio::test]
async fn two_processes_over_one_store_send_one_start_and_one_daily_of_both_counts() {
    let collector = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/report"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&collector)
        .await;
    let url = format!("{}/v1/report", collector.uri());
    let store = Arc::new(Shared::new());
    let first = shared_reporter(&url, &store, 3);
    let second = shared_reporter(&url, &store, 2);
    assert_eq!(
        StartOutcome::Sent(StatusCode::ACCEPTED),
        first.send_start().await.expect("the start is sent")
    );
    assert_eq!(
        StartOutcome::RateLimited,
        second
            .send_start()
            .await
            .expect("the other process skips it")
    );
    assert_eq!(
        StatusCode::ACCEPTED,
        first.send_daily().await.expect("the daily report is sent")
    );
    assert!(
        matches!(second.send_daily().await, Err(ReportError::SentElsewhere)),
        "the other process sent today"
    );
    assert_eq!(
        0,
        second.window.take().requests(),
        "its counts wait in the store"
    );
    store.next_day();
    assert_eq!(
        StatusCode::ACCEPTED,
        first
            .send_daily()
            .await
            .expect("the next daily report is sent")
    );
    let received = collector
        .received_requests()
        .await
        .expect("the requests are recorded");
    let bodies: Vec<Value> = received
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("JSON"))
        .collect();
    assert_eq!(3, bodies.len(), "one start and two daily reports");
    let ids: Vec<&Value> = bodies.iter().map(|body| &body["instance_id"]).collect();
    assert!(
        ids.iter()
            .all(|id| id.as_str() == Some(store.id.hyphenated().to_string().as_str()))
    );
    let histogram = |body: &Value| -> u64 {
        body["metrics"]["window_24h"]["groups"]["pdqm_search"]["histogram"]
            .as_array()
            .map_or(0, |buckets| buckets.iter().filter_map(Value::as_u64).sum())
    };
    assert_eq!(
        vec![3, 2],
        bodies.iter().skip(1).map(histogram).collect::<Vec<u64>>(),
        "the second daily report carries the other process's counts"
    );
}

#[test]
fn the_report_runs_only_when_switched_on_outside_ci_with_a_state_directory() {
    let dir = tempfile::tempdir().expect("a state directory");
    let on = UsageReportSection {
        state_dir: Some(dir.path().to_path_buf()),
        ..UsageReportSection::default()
    };
    assert!(prepare(&on, None).is_ok());
    let off = UsageReportSection {
        enabled: false,
        ..on.clone()
    };
    assert!(matches!(prepare(&off, None), Err(Off::Switch)));
    assert!(matches!(prepare(&on, Some("CI")), Err(Off::Ci("CI"))));
    let none = UsageReportSection::default();
    assert!(matches!(prepare(&none, None), Err(Off::NoStateDir)));
    let file = dir.path().join("file");
    std::fs::write(&file, b"").expect("a file");
    let unusable = UsageReportSection {
        state_dir: Some(file),
        ..UsageReportSection::default()
    };
    assert!(matches!(
        prepare(&unusable, None),
        Err(Off::StateDir { .. })
    ));
}

#[test]
fn the_boot_line_names_the_switch_and_the_page() {
    let line = announcement(&NOTICE, &Err(Off::Switch));
    assert!(
        line.starts_with("usage report OFF: switched off (FERROSYS__USAGE_REPORT__ENABLED=false)"),
        "{line}"
    );
    assert!(
        line.ends_with("Details: https://ferrohealth.eu/docs/usage-report.html"),
        "{line}"
    );
    let line = announcement(&NOTICE, &Err(Off::NoStateDir));
    assert!(
        line.contains("FERROSYS__USAGE_REPORT__STATE_DIR is unset"),
        "{line}"
    );
}

#[test]
fn the_host_memory_reads_meminfo() {
    assert_eq!(
        Some(16_384_000 * 1024),
        meminfo_total_bytes("MemTotal:       16384000 kB\nMemFree: 1 kB\n")
    );
    assert_eq!(None, meminfo_total_bytes("MemFree: 1 kB\n"));
}
