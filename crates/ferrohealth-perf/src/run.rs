// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One run against a server: the idle phase, the load phase, and with
//! `--update` a second load phase for the A/A noise floor; then the
//! judgement, the report files and the baseline.
//!
//! `--update` writes the baseline only from a run that passes: a breach or
//! a run that cannot be judged leaves the file as it was. No specification
//! governs the run: our own design.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ferrohealth_secret::secret_url::SecretUrl;
use thiserror::Error;
use tokio::time::{Instant, sleep, sleep_until};
use url::Url;

use crate::baseline::{self, Baseline, BaselineError, Change, Update};
use crate::compare::{Outcome, Verdict, judge};
use crate::database::{DatabaseError, Transactions};
use crate::fingerprint::Fingerprint;
use crate::load::{self, LoadError};
use crate::metric;
use crate::plan::Plan;
use crate::process::{Probe, ProcessError, Sample, Target};
use crate::quantile::{QuantileName, quantile};
use crate::report::{
    GroupReport, IdleReport, LoadReport, PlanIdentity, Report, ReportError, SCHEMA, TargetInfo,
};
use crate::summary;

/// What a run is given.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The plan.
    pub plan: Plan,
    /// The server's process or cgroup.
    pub target: Target,
    /// The server's base URL.
    pub base_url: SecretUrl,
    /// The server's database, when its transactions are counted.
    pub database_url: Option<SecretUrl>,
    /// The baseline file.
    pub baseline: PathBuf,
    /// The directory the reports are written to.
    pub out_dir: PathBuf,
    /// Whether the run writes the baseline.
    pub update: bool,
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// The report.
    pub report: Report,
    /// Its judgement.
    pub verdict: Verdict,
    /// The dated report file.
    pub report_path: PathBuf,
    /// The Markdown summary.
    pub summary: String,
    /// What `--update` did to the baseline, when it wrote it.
    pub changes: Option<Vec<Change>>,
}

/// A run that could not be carried out: a harness or setup error.
#[derive(Debug, Error)]
pub enum RunError {
    /// The base URL is not a URL.
    #[error("the base URL {0} is not a URL")]
    BaseUrl(String, #[source] url::ParseError),
    /// The HTTP client could not be built.
    #[error("the HTTP client cannot be built")]
    Client(#[source] reqwest::Error),
    /// The baseline could not be read or written.
    #[error(transparent)]
    Baseline(#[from] BaselineError),
    /// The server's process or cgroup could not be read.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// The database could not be read.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The load phase failed.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// The report could not be built or written.
    #[error(transparent)]
    Report(#[from] ReportError),
    /// A sampling task failed.
    #[error("a sampling task failed")]
    Task(#[source] tokio::task::JoinError),
    /// The transaction count fell, so the statistics were reset mid-run.
    #[error(
        "pg_stat_database fell from {before} to {after} transactions: its statistics were reset"
    )]
    StatsReset {
        /// The count before.
        before: u64,
        /// The count after.
        after: u64,
    },
}

/// The time now, RFC 3339 in UTC.
fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// The UTC time as a file name stamp, `20261106T101500Z`.
fn stamp() -> String {
    jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// The server and the database a run reads.
#[derive(Debug)]
struct Instruments {
    probe: Probe,
    database: Option<Transactions>,
}

impl Instruments {
    async fn sample(&self) -> Result<Sample, RunError> {
        let probe = self.probe.clone();
        tokio::task::spawn_blocking(move || probe.sample())
            .await
            .map_err(RunError::Task)?
            .map_err(RunError::from)
    }

    async fn transactions(&self) -> Result<Option<u64>, RunError> {
        match &self.database {
            Some(database) => Ok(Some(database.count().await?)),
            None => Ok(None),
        }
    }
}

/// The change in the transaction count from `before` to `after`.
fn delta(before: Option<u64>, after: Option<u64>) -> Result<Option<u64>, RunError> {
    match (before, after) {
        (Some(before), Some(after)) => after
            .checked_sub(before)
            .map(Some)
            .ok_or(RunError::StatsReset { before, after }),
        _ => Ok(None),
    }
}

/// The CPU time between two samples.
fn cpu_between(first: Sample, last: Sample) -> Option<Duration> {
    Some(last.cpu?.saturating_sub(first.cpu?))
}

async fn idle_phase(plan: &Plan, instruments: &Instruments) -> Result<IdleReport, RunError> {
    let idle = &plan.idle;
    sleep(Duration::from_secs(idle.settle_secs)).await;
    let first = instruments.sample().await?;
    let before = instruments.transactions().await?;
    let started = Instant::now();
    sleep(Duration::from_secs(idle.duration_secs)).await;
    let last = instruments.sample().await?;
    let after = instruments.transactions().await?;
    let elapsed = started.elapsed().as_secs_f64();
    let per_second = |value: f64| value / elapsed;
    Ok(IdleReport {
        duration_secs: elapsed,
        cpu_percent: cpu_between(first, last).map(|cpu| per_second(cpu.as_secs_f64()) * 100.0),
        max_cpu_percent: idle.max_cpu_percent,
        transactions_per_second: delta(before, after)?
            .map(|count| per_second(crate::quantile::exact_f64(count))),
        max_transactions_per_second: idle.max_transactions_per_second,
        memory_start_bytes: first.memory_bytes,
        memory_end_bytes: last.memory_bytes,
    })
}

/// Samples the memory every `interval` on its own thread until stopped, and
/// keeps the peak.
#[derive(Debug)]
struct Peak {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<Option<u64>, ProcessError>>,
}

impl Peak {
    fn start(probe: Probe, interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut peak: Option<u64> = None;
            loop {
                if let Some(bytes) = probe.sample()?.memory_bytes {
                    peak = Some(peak.map_or(bytes, |p| p.max(bytes)));
                }
                if flag.load(Ordering::Acquire) {
                    return Ok(peak);
                }
                std::thread::sleep(interval);
            }
        });
        Self { stop, thread }
    }

    fn finish(self) -> Result<Option<u64>, RunError> {
        self.stop.store(true, Ordering::Release);
        match self.thread.join() {
            Ok(result) => result.map_err(RunError::from),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

async fn load_phase(
    plan: &Plan,
    base: &Url,
    client: &reqwest::Client,
    instruments: &Instruments,
) -> Result<LoadReport, RunError> {
    let load = &plan.load;
    let before = instruments.transactions().await?;
    let start = Instant::now()
        .checked_add(Duration::from_millis(50))
        .unwrap_or_else(Instant::now);
    let measured_start = start.checked_add(load.warmup()).unwrap_or(start);
    let measured_end = measured_start
        .checked_add(load.duration())
        .unwrap_or(measured_start);
    let sample_at = |at: Instant| {
        let probe = instruments.probe.clone();
        tokio::spawn(async move {
            sleep_until(at).await;
            tokio::task::spawn_blocking(move || probe.sample()).await
        })
    };
    let first = sample_at(measured_start);
    let last = sample_at(measured_end);
    let peak = Peak::start(
        instruments.probe.clone(),
        Duration::from_millis(load.sample_ms),
    );
    let driven = load::drive(plan, base, client, start).await;
    let peak = peak.finish()?;
    let outcome = driven?;
    let sample = |handle: tokio::task::JoinHandle<_>| async move {
        let joined: Result<Result<Sample, ProcessError>, tokio::task::JoinError> =
            handle.await.map_err(RunError::Task)?;
        joined.map_err(RunError::Task)?.map_err(RunError::from)
    };
    let (first, last) = (sample(first).await?, sample(last).await?);
    sleep(Duration::from_secs(load.settle_secs)).await;
    let after = instruments.transactions().await?;
    let mut groups = std::collections::BTreeMap::new();
    for (name, record) in &outcome.groups {
        let group = GroupReport::new(record.answered, &record.first, &record.second)?;
        groups.insert(name.clone(), group);
    }
    Ok(LoadReport {
        measured_secs: load.duration().as_secs_f64(),
        planned: outcome.planned,
        sent: outcome.sent,
        saturated: outcome.saturated,
        lag_p99_ms: quantile(&outcome.lag, QuantileName::P99).value_ms(),
        max_lag_ms: load.max_lag_ms,
        peak_memory_bytes: peak,
        cpu_ms: cpu_between(first, last).map(|cpu| cpu.as_secs_f64() * 1_000.0),
        transactions: delta(before, after)?,
        groups,
        failures: outcome.failures,
    })
}

/// Carries out one run.
///
/// # Errors
///
/// A [`RunError`] when the run cannot be carried out: a base URL that is
/// not a URL, a baseline that does not read, a server or database that
/// cannot be read, a load phase that fails, or a report or baseline that
/// cannot be written.
pub async fn run(options: &RunOptions) -> Result<RunResult, RunError> {
    let old = Baseline::read(&options.baseline)?;
    let base = Url::parse(options.base_url.expose())
        .map_err(|source| RunError::BaseUrl(options.base_url.redacted(), source))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(RunError::Client)?;
    let probe = Probe::new(options.target.clone());
    probe.sample()?;
    let database = match &options.database_url {
        Some(url) => Some(Transactions::connect(url).await?),
        None => None,
    };
    let instruments = Instruments { probe, database };
    let started = now();
    let plan = &options.plan;
    let idle = idle_phase(plan, &instruments).await?;
    let load = load_phase(plan, &base, &client, &instruments).await?;
    let calibration = if options.update {
        Some(load_phase(plan, &base, &client, &instruments).await?)
    } else {
        None
    };
    let report = Report {
        schema: SCHEMA,
        tool: env!("CARGO_PKG_VERSION").to_owned(),
        started,
        finished: now(),
        host: Fingerprint::detect(),
        target: TargetInfo {
            process: options.target.describe(),
            base_url: options.base_url.redacted(),
            database: instruments.database.as_ref().map(|d| d.name().to_owned()),
        },
        plan: identity(plan),
        idle,
        load,
        calibration,
    };
    conclude(report, old.as_ref(), options)
}

/// What makes runs of `plan` comparable.
#[must_use]
pub fn identity(plan: &Plan) -> PlanIdentity {
    PlanIdentity {
        rate: plan.load.rate,
        warmup_secs: plan.load.warmup_secs,
        duration_secs: plan.load.duration_secs,
        requests: u64::try_from(plan.requests.len()).unwrap_or(u64::MAX),
        groups: plan.groups(),
    }
}

/// Judges `report`, writes it, and with `--update` writes the baseline from
/// a run that passed.
///
/// # Errors
///
/// [`RunError::Report`] or [`RunError::Baseline`] when a file cannot be
/// written or a histogram does not decode.
pub fn conclude(
    report: Report,
    old: Option<&Baseline>,
    options: &RunOptions,
) -> Result<RunResult, RunError> {
    let verdict = judge(&report, old, options.update)?;
    let report_path = report.write(&options.out_dir, &stamp())?;
    let mut text = summary::markdown(&report, &verdict);
    let mut changes = None;
    if options.update {
        if verdict.outcome == Outcome::Pass {
            let metrics = metric::of(&report.load)?;
            let noise = match &report.calibration {
                Some(second) => baseline::noise_floor(&metrics, &metric::of(second)?),
                None => std::collections::BTreeMap::new(),
            };
            let written = now();
            let (baseline, done) = baseline::updated(
                old,
                &Update {
                    metrics: &metrics,
                    noise,
                    plan: &report.plan,
                    host: &report.host,
                    tool: &report.tool,
                    written: &written,
                },
            );
            baseline.write(&options.baseline)?;
            text.push_str(&changes_markdown(&options.baseline, &done));
            changes = Some(done);
        } else {
            text.push_str("### Baseline\n\n--update was refused: only a run that passes writes the baseline.\n");
        }
    }
    Ok(RunResult {
        report,
        verdict,
        report_path,
        summary: text,
        changes,
    })
}

fn changes_markdown(path: &Path, changes: &[Change]) -> String {
    let mut text = format!("### Baseline\n\nWritten to `{}`:\n\n", path.display());
    for change in changes {
        let item = match change {
            Change::Added(metric, value) => format!("{metric} added at {value:.3}"),
            Change::Tightened { metric, from, to } => {
                format!("{metric} tightened from {from:.3} to {to:.3}")
            }
            Change::Kept(metric, value) => format!("{metric} kept at {value:.3}"),
            Change::Rewritten => "written afresh for the new plan".to_owned(),
        };
        text.push_str("- ");
        text.push_str(&item);
        text.push('\n');
    }
    text
}
