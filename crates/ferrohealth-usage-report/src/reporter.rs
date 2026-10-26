// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The reporter: decides whether the report runs, assembles each report,
//! sends it and schedules the start and daily events.
//!
//! The channel is outbound only: nothing the collector answers is read
//! beyond its status, and no answer changes configuration or behaviour. A
//! send is bounded by timeouts and a body cap, a refusal is logged with the
//! collector's status, and a failed daily report is tried again an hour
//! later with its counts put back. The task runs detached from boot and
//! readiness, so a slow or unreachable collector reaches neither.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::StatusCode;
use serde::Serialize;
use tokio::task::JoinHandle;

use crate::envelope::{
    CpuBucket, Event, LicenceGrant, MAX_UPTIME_S, MemoryBucket, Product, Report, SchemaV2,
};
use crate::section::{Deployment, UsageReportSection};
use crate::state::{Instance, StateDir, StateError};
use crate::window::Window;

/// The largest report body the collector takes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// How long after the start the first daily report goes out.
pub const FIRST_DAILY_AFTER: Duration = Duration::from_mins(10);

/// How long after a failed daily report it is tried again.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_hours(1);

/// The connection timeout of a send.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The whole timeout of a send.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// What a product contributes: its name in the envelope and its `status`.
///
/// The status is the product's own closed type, which serializes as an
/// object of at most 32 members whose keys follow the envelope's key pattern
/// and whose values are booleans, counts, short lower-case tokens or arrays
/// of them; it never carries free text, an identifier or a URL.
pub trait ProductSection: Send + Sync + 'static {
    /// The status object.
    type Status: Serialize + Send + Sync;

    /// The product the reports name.
    const PRODUCT: Product;

    /// The product's state facts now.
    fn status(&self) -> impl Future<Output = Self::Status> + Send;
}

/// What a product says in its boot line about the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Notice {
    /// What the reports carry, in one sentence fragment, such as "instance
    /// id, version, licence type, deployment, uptime, host size and coarse
    /// request latency per route group".
    pub contents: &'static str,
    /// The setting that switches the report off, such as
    /// `FERROPIX__USAGE_REPORT__ENABLED=false`.
    pub switch: &'static str,
    /// The variable that names the state directory.
    pub state_dir_variable: &'static str,
    /// The page that documents the report.
    pub url: &'static str,
}

/// Why the report does not run.
#[derive(Debug)]
pub enum Off {
    /// The deployment switched it off.
    Switch,
    /// A CI run, named by the variable that marks it.
    Ci(&'static str),
    /// No state directory is configured.
    NoStateDir,
    /// The state directory cannot serve.
    StateDir {
        /// The directory.
        path: PathBuf,
        /// What failed.
        error: StateError,
    },
}

/// A report that runs: the state directory and the instance it holds.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// The state directory.
    pub state: StateDir,
    /// The instance id.
    pub instance: Instance,
}

/// Decides whether the report runs under `section`, given the CI variable
/// set, if any, and opens the state directory when it does.
///
/// # Errors
///
/// Returns the [`Off`] reason when the report does not run.
pub fn prepare(section: &UsageReportSection, ci: Option<&'static str>) -> Result<Prepared, Off> {
    if !section.enabled {
        return Err(Off::Switch);
    }
    if let Some(variable) = ci {
        return Err(Off::Ci(variable));
    }
    let Some(root) = &section.state_dir else {
        return Err(Off::NoStateDir);
    };
    let failed = |error| Off::StateDir {
        path: root.clone(),
        error,
    };
    let state = StateDir::open(root).map_err(failed)?;
    let instance = state.instance().map_err(failed)?;
    Ok(Prepared { state, instance })
}

/// The boot line that says whether the report runs and how to switch it.
#[must_use]
pub fn announcement(notice: &Notice, decision: &Result<Prepared, Off>) -> String {
    let Notice {
        contents,
        switch,
        state_dir_variable,
        url,
    } = notice;
    match decision {
        Ok(_) => format!(
            "usage report ON: {contents}, at each start and once a day; no clinical content, \
             identifiers, URLs or request paths. Off: {switch}. Details: {url}"
        ),
        Err(Off::Switch) => {
            format!("usage report OFF: switched off ({switch}); nothing is sent. Details: {url}")
        }
        Err(Off::Ci(variable)) => {
            format!("usage report OFF: CI detected ({variable}); nothing is sent. Details: {url}")
        }
        Err(Off::NoStateDir) => format!(
            "usage report OFF: no state directory ({state_dir_variable} is unset), and without a \
             stable instance id each restart would count as a new installation; nothing is sent. \
             On: mount a writable volume and set {state_dir_variable} to it. Details: {url}"
        ),
        Err(Off::StateDir { .. }) => format!(
            "usage report OFF: the state directory ({state_dir_variable}) cannot be written, and \
             without a stable instance id each restart would count as a new installation; \
             nothing is sent. On: mount a writable volume and set {state_dir_variable} to it. \
             Details: {url}"
        ),
    }
}

/// The facts of the build a report names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The product's version, in Semantic Versioning.
    pub version: String,
    /// The commit, 7 to 40 lower-case hex digits (see
    /// [`crate::envelope::report_git_sha`]).
    pub git_sha: String,
    /// The licence grant.
    pub licence: LicenceGrant,
}

/// A report could not be sent.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReportError {
    /// The HTTP client could not be built.
    #[error("cannot build the HTTP client for the collector")]
    Client(#[source] reqwest::Error),
    /// The collector could not be reached.
    #[error("cannot reach the collector")]
    Request(#[source] reqwest::Error),
    /// The report did not serialize.
    #[error("the report does not serialize")]
    Json(#[source] serde_json::Error),
    /// The report is larger than the collector takes.
    #[error("the report body of {0} bytes exceeds the {MAX_BODY_BYTES}-byte limit")]
    TooLarge(usize),
    /// The state directory failed.
    #[error("the state directory failed")]
    State(#[source] StateError),
    /// The state directory task failed.
    #[error("the state directory task failed")]
    Task(#[source] tokio::task::JoinError),
}

/// How a start report ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// Sent; the collector answered this status.
    Sent(StatusCode),
    /// Skipped: a start report of this instance went out within ten minutes.
    RateLimited,
}

/// The reporter of one product.
#[derive(Debug)]
pub struct Reporter<P> {
    endpoint: url::Url,
    client: reqwest::Client,
    identity: Identity,
    deployment: Deployment,
    prepared: Prepared,
    product: Arc<P>,
    window: Arc<Window>,
    started: Instant,
}

impl<P: ProductSection> Reporter<P> {
    /// A reporter to `section`'s collector for `product`, whose daily
    /// metrics come from `window`, counting uptime from `started`.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::Client`] when the HTTP client cannot be built.
    pub fn new(
        section: &UsageReportSection,
        identity: Identity,
        prepared: Prepared,
        product: Arc<P>,
        window: Arc<Window>,
        started: Instant,
    ) -> Result<Self, ReportError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("{}/{}", product_name(P::PRODUCT), identity.version))
            .build()
            .map_err(ReportError::Client)?;
        Ok(Self {
            endpoint: section.endpoint.url().clone(),
            client,
            identity,
            deployment: section.deployment,
            prepared,
            product,
            window,
            started,
        })
    }

    /// The start report.
    pub async fn start_report(&self) -> Report<P::Status> {
        self.assemble(Event::Start {
            first_start: self.prepared.instance.created,
        })
        .await
    }

    /// The daily report of the window's counts so far, leaving them in the
    /// window.
    pub async fn daily_report(&self) -> Report<P::Status> {
        let counts = self.window.take();
        self.window.restore(&counts);
        self.assemble(Event::Daily {
            metrics: Box::new(counts.to_metrics()),
        })
        .await
    }

    /// Sends the start report, unless one went out within ten minutes.
    ///
    /// # Errors
    ///
    /// Returns the [`ReportError`] of the state directory or of the send.
    pub async fn send_start(&self) -> Result<StartOutcome, ReportError> {
        let state = self.prepared.state.clone();
        let claimed =
            tokio::task::spawn_blocking(move || state.claim_start(jiff::Timestamp::now()))
                .await
                .map_err(ReportError::Task)?
                .map_err(ReportError::State)?;
        if !claimed {
            return Ok(StartOutcome::RateLimited);
        }
        let report = self.start_report().await;
        Ok(StartOutcome::Sent(self.post(&report).await?))
    }

    /// Sends the daily report and empties the window; a report that does not
    /// reach the collector puts its counts back.
    ///
    /// # Errors
    ///
    /// Returns the [`ReportError`] of the send.
    pub async fn send_daily(&self) -> Result<StatusCode, ReportError> {
        let counts = self.window.take();
        let report = self
            .assemble(Event::Daily {
                metrics: Box::new(counts.to_metrics()),
            })
            .await;
        let sent = self.post(&report).await;
        if sent.is_err() {
            self.window.restore(&counts);
        }
        sent
    }

    /// Runs the schedule on a detached task: the start report now, the first
    /// daily report ten minutes later, then one a day.
    #[must_use]
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            match self.send_start().await {
                Ok(StartOutcome::Sent(status)) => log_answer("start", status),
                Ok(StartOutcome::RateLimited) => {
                    tracing::debug!(
                        "usage report: a start report of this instance went out in the last ten minutes"
                    );
                }
                Err(error) => log_failure("start", &error),
            }
            let mut wait = FIRST_DAILY_AFTER;
            loop {
                tokio::time::sleep(wait).await;
                wait = match self.send_daily().await {
                    Ok(status) => {
                        log_answer("daily", status);
                        next_daily_wait()
                    }
                    Err(error) => {
                        log_failure("daily", &error);
                        RETRY_AFTER_FAILURE
                    }
                };
            }
        })
    }

    async fn assemble(&self, event: Event) -> Report<P::Status> {
        Report {
            schema: SchemaV2,
            event,
            product: P::PRODUCT,
            instance_id: self.prepared.instance.id.hyphenated().to_string(),
            version: self.identity.version.clone(),
            git_sha: self.identity.git_sha.clone(),
            licence: self.identity.licence,
            deployment: self.deployment,
            uptime_s: self.started.elapsed().as_secs().min(MAX_UPTIME_S),
            cpu_bucket: CpuBucket::of(
                std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            ),
            memory_bucket: MemoryBucket::of(host_memory_bytes().unwrap_or(0)),
            status: self.product.status().await,
        }
    }

    async fn post(&self, report: &Report<P::Status>) -> Result<StatusCode, ReportError> {
        let body = serde_json::to_vec(report).map_err(ReportError::Json)?;
        if body.len() > MAX_BODY_BYTES {
            return Err(ReportError::TooLarge(body.len()));
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(ReportError::Request)?;
        Ok(response.status())
    }
}

/// Starts the report for `product`: decides, says so in one log line, and
/// spawns the schedule when the report runs.
///
/// `ci` is the CI variable [`crate::section::ci_of`] found, if any. A
/// failure to build the client is logged, and nothing runs.
pub async fn start<P: ProductSection>(
    section: &UsageReportSection,
    notice: &Notice,
    identity: Identity,
    product: Arc<P>,
    window: Arc<Window>,
    started: Instant,
    ci: Option<&'static str>,
) -> Option<JoinHandle<()>> {
    let owned = section.clone();
    let decision = match tokio::task::spawn_blocking(move || prepare(&owned, ci)).await {
        Ok(decision) => decision,
        Err(error) => {
            tracing::warn!(error = %error, "usage report: the state directory task failed");
            return None;
        }
    };
    let line = announcement(notice, &decision);
    match &decision {
        Ok(_) => tracing::info!(endpoint = %section.endpoint.url(), "{line}"),
        Err(Off::StateDir { path, error }) => {
            tracing::info!(path = %path.display(), error = %error, "{line}");
        }
        Err(_) => tracing::info!("{line}"),
    }
    let prepared = decision.ok()?;
    match Reporter::new(section, identity, prepared, product, window, started) {
        Ok(reporter) => Some(reporter.spawn()),
        Err(error) => {
            log_failure("start", &error);
            None
        }
    }
}

/// The lower-case product name of the user agent.
const fn product_name(product: Product) -> &'static str {
    match product {
        Product::Ehr => "ferroehr",
        Product::Term => "ferroterm",
        Product::Bridge => "ferrobridge",
        Product::Fed => "ferrofed",
        Product::Pix => "ferropix",
    }
}

fn log_answer(event: &'static str, status: StatusCode) {
    if status.is_success() {
        tracing::debug!(event, status = status.as_u16(), "usage report: stored");
    } else {
        tracing::warn!(
            event,
            status = status.as_u16(),
            "usage report: the collector refused the report; it is not sent again"
        );
    }
}

fn log_failure(event: &'static str, error: &ReportError) {
    let mut chain = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    tracing::warn!(
        event,
        error = chain.as_str(),
        "usage report: the report was not sent"
    );
}

fn next_daily_wait() -> Duration {
    // NOTE: a v4 UUID is 122 random bits from the operating system
    // (<https://docs.rs/uuid/latest/uuid/struct.Uuid.html#method.new_v4>).
    let spread = u64::try_from(uuid::Uuid::new_v4().as_u128() % 3601).unwrap_or(0);
    Duration::from_secs(23 * 3600 + 1800 + spread)
}

fn host_memory_bytes() -> Option<u64> {
    // NOTE: proc(5) `/proc/meminfo` and the cgroup v2 and v1 memory files;
    // absent off Linux, where the report states the smallest bucket.
    let total = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| meminfo_total_bytes(&text));
    let limit = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
        .or_else(|_absent| std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    match (total, limit) {
        (Some(total), Some(limit)) => Some(total.min(limit)),
        (total, limit) => total.or(limit),
    }
}

fn meminfo_total_bytes(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .map(|kib| kib.saturating_mul(1024))
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use http::{Method, StatusCode};
    use serde_json::Value;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{
        Identity, Notice, Off, ProductSection, Reporter, StartOutcome, announcement,
        meminfo_total_bytes, prepare,
    };
    use crate::envelope::{Key, LicenceGrant, Product};
    use crate::section::{Endpoint, UsageReportSection};
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
        let start = serde_json::to_value(reporter.start_report().await)
            .expect("the start report serializes");
        assert_valid(&start);
        assert_eq!("start", start["event"]);
        assert_eq!(true, start["first_start"], "the first start created the id");
        assert_eq!("ferropix", start["product"]);
        let daily = serde_json::to_value(reporter.daily_report().await)
            .expect("the daily report serializes");
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
            line.starts_with(
                "usage report OFF: switched off (FERROSYS__USAGE_REPORT__ENABLED=false)"
            ),
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
}
