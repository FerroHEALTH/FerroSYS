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
use crate::state::{DailyClaim, Instance, StateDir, StateError, Store, StoreError};
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
        switch,
        state_dir_variable,
        url,
        ..
    } = notice;
    match decision {
        Ok(_) => on_line(notice),
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
    /// The state store failed.
    #[error("the state store failed")]
    Store(#[source] StoreError),
    /// Another process of this installation sent the daily report within
    /// the interval; this process's counts wait in the store for the next.
    #[error("another process of this installation sent the daily report")]
    SentElsewhere,
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
    instance: Instance,
    store: Arc<dyn Store>,
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
        let Prepared { state, instance } = prepared;
        Self::with_store(
            section,
            identity,
            (instance, Arc::new(state)),
            product,
            window,
            started,
        )
    }

    /// A reporter as [`Reporter::new`] builds one, over the store `store`
    /// that holds the installation `instance`, such as a database several
    /// replicas share.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::Client`] when the HTTP client cannot be built.
    pub fn with_store(
        section: &UsageReportSection,
        identity: Identity,
        (instance, store): (Instance, Arc<dyn Store>),
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
            instance,
            store,
            product,
            window,
            started,
        })
    }

    /// The start report.
    pub async fn start_report(&self) -> Report<P::Status> {
        self.assemble(Event::Start {
            first_start: self.instance.created,
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
    /// Returns the [`ReportError`] of the state store or of the send.
    pub async fn send_start(&self) -> Result<StartOutcome, ReportError> {
        let claimed = self
            .store
            .claim_start(jiff::Timestamp::now())
            .await
            .map_err(ReportError::Store)?;
        if !claimed {
            return Ok(StartOutcome::RateLimited);
        }
        let report = self.start_report().await;
        Ok(StartOutcome::Sent(self.post(&report).await?))
    }

    /// Hands the window's counts to the store and, when this process holds
    /// the daily claim, sends the daily report of every process's counts;
    /// a report that does not reach the collector puts its counts back.
    ///
    /// # Errors
    ///
    /// Returns [`ReportError::SentElsewhere`] when another process sent the
    /// daily report within the interval, and the [`ReportError`] of the
    /// state store or of the send otherwise. A store that refuses the claim
    /// leaves the counts in the window.
    pub async fn send_daily(&self) -> Result<StatusCode, ReportError> {
        let local = self.window.take();
        let claim = match self
            .store
            .claim_daily(jiff::Timestamp::now(), local.clone())
            .await
        {
            Ok(claim) => claim,
            Err(error) => {
                self.window.restore(&local);
                return Err(ReportError::Store(error));
            }
        };
        let DailyClaim::Taken(counts) = claim else {
            return Err(ReportError::SentElsewhere);
        };
        let report = self
            .assemble(Event::Daily {
                metrics: Box::new(counts.to_metrics()),
            })
            .await;
        match self.post(&report).await {
            Ok(status) => Ok(status),
            Err(error) => {
                match self.store.restore(counts.clone()).await {
                    Ok(Some(kept)) => self.window.restore(&kept),
                    Ok(None) => {}
                    Err(restore) => {
                        log_failure("daily", &ReportError::Store(restore));
                        self.window.restore(&counts);
                    }
                }
                Err(error)
            }
        }
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
                    Err(ReportError::SentElsewhere) => {
                        tracing::debug!(
                            "usage report: another process of this installation sent the daily report"
                        );
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
            instance_id: self.instance.id.hyphenated().to_string(),
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

/// Starts the report for `product` over the store `store`, as [`start`] does
/// over the state directory: decides, says so in one log line, and spawns
/// the schedule when the report runs.
///
/// A store that cannot give the installation id is logged, and nothing
/// runs.
#[expect(
    clippy::too_many_arguments,
    reason = "the arguments of `start` and the store; a parameter struct would only rename them"
)]
pub async fn start_with_store<P: ProductSection>(
    section: &UsageReportSection,
    notice: &Notice,
    identity: Identity,
    store: Arc<dyn Store>,
    product: Arc<P>,
    window: Arc<Window>,
    started: Instant,
    ci: Option<&'static str>,
) -> Option<JoinHandle<()>> {
    let off = if section.enabled {
        ci.map(Off::Ci)
    } else {
        Some(Off::Switch)
    };
    if let Some(off) = off {
        tracing::info!("{}", announcement(notice, &Err(off)));
        return None;
    }
    let instance = match store.instance().await {
        Ok(instance) => instance,
        Err(error) => {
            log_failure("start", &ReportError::Store(error));
            return None;
        }
    };
    tracing::info!(endpoint = %section.endpoint.url(), "{}", on_line(notice));
    match Reporter::with_store(
        section,
        identity,
        (instance, store),
        product,
        window,
        started,
    ) {
        Ok(reporter) => Some(reporter.spawn()),
        Err(error) => {
            log_failure("start", &error);
            None
        }
    }
}

/// The boot line of a report that runs.
fn on_line(notice: &Notice) -> String {
    format!(
        "usage report ON: {}, at each start and once a day; no clinical content, identifiers, \
         URLs or request paths. Off: {}. Details: {}",
        notice.contents, notice.switch, notice.url
    )
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
mod tests;
