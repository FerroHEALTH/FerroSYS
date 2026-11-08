// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One run against a server: the idle phase, the load phase, the
//! judgement, the report files and, with `--update`, the baseline.
//!
//! An update measures further load phases, each after its own warm-up, and
//! writes the entry from their medians and spread. A run that breaches a
//! load metric runs up to two fresh load phases to confirm it.
//!
//! `--update` writes the baseline entry of the runner's host shape only from
//! a run that passes, and leaves every other entry as it was: a breach or a
//! run that cannot be judged leaves the file as it was. No specification
//! governs the run: our own design.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ferrohealth_secret::secret_url::SecretUrl;
use thiserror::Error;
use tokio::time::{Instant, sleep, sleep_until};
use url::Url;

use crate::baseline::{self, BaselineError, Baselines, Change, Update};
use crate::compare::{Outcome, Verdict, judge_file};
use crate::credentials::{CredentialError, Credentials};
use crate::database::{DatabaseError, Transactions, WITNESS_INTERVAL};
use crate::fingerprint::Fingerprint;
use crate::load::{self, Endpoints, LoadError};
use crate::metric;
use crate::mllp::MllpError;
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
    /// The server's MLLP listener, `host:port`, in place of the plan's
    /// `[mllp] address`.
    pub mllp_address: Option<String>,
    /// The server's database, when its transactions are counted.
    pub database_url: Option<SecretUrl>,
    /// The baseline file.
    pub baseline: PathBuf,
    /// The directory the reports are written to.
    pub out_dir: PathBuf,
    /// Whether the run writes the baseline.
    pub update: bool,
    /// The load phases an update measures, at least two: the entry takes
    /// their median and their spread is its noise floor.
    pub runs: u32,
}

/// The load phases an update measures unless told otherwise.
pub const DEFAULT_RUNS: u32 = 5;

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
    /// A credential file of the plan could not be used.
    #[error(transparent)]
    Credential(#[from] CredentialError),
    /// The server's process or cgroup could not be read.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// The database could not be read.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The load phase failed.
    #[error(transparent)]
    Load(#[from] LoadError),
    /// The MLLP listener of the plan's messages cannot be used.
    #[error(transparent)]
    Mllp(#[from] MllpError),
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
pub(crate) fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// The UTC time as a file name stamp, `20261106T101500Z`.
pub(crate) fn stamp() -> String {
    jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string()
}

/// The server and the database a run reads.
#[derive(Debug)]
pub(crate) struct Instruments {
    pub(crate) probe: Probe,
    pub(crate) database: Option<Transactions>,
}

impl Instruments {
    /// Reads `target` once, so a server that cannot be read refuses the run
    /// before any phase, and connects to `database` when one is given.
    pub(crate) async fn open(
        target: &Target,
        database: Option<&SecretUrl>,
    ) -> Result<Self, RunError> {
        let probe = Probe::new(target.clone());
        probe.sample()?;
        let database = match database {
            Some(url) => Some(Transactions::connect(url).await?),
            None => None,
        };
        Ok(Self { probe, database })
    }

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

    /// Waits `duration`, watching the database's activity when there is
    /// one, and returns the transactions it witnessed.
    async fn watch(&self, duration: Duration) -> Result<Option<u64>, RunError> {
        if let Some(database) = &self.database {
            return Ok(Some(database.witness(duration, WITNESS_INTERVAL).await?));
        }
        sleep(duration).await;
        Ok(None)
    }
}

/// The server's URL, parsed; its credentials are redacted in the error.
pub(crate) fn server_url(url: &SecretUrl) -> Result<Url, RunError> {
    Url::parse(url.expose()).map_err(|source| RunError::BaseUrl(url.redacted(), source))
}

/// The credentials `plan` names and the HTTP client that presents them.
pub(crate) fn client(plan: &Plan) -> Result<(Credentials, reqwest::Client), RunError> {
    let credentials = Credentials::load(plan)?;
    let client = credentials
        .apply(reqwest::Client::builder().no_proxy())
        .build()
        .map_err(RunError::Client)?;
    Ok((credentials, client))
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

pub(crate) async fn idle_phase(
    plan: &Plan,
    instruments: &Instruments,
) -> Result<IdleReport, RunError> {
    let idle = &plan.idle;
    sleep(Duration::from_secs(idle.settle_secs)).await;
    let first = instruments.sample().await?;
    let before = instruments.transactions().await?;
    let started = Instant::now();
    let witnessed = instruments
        .watch(Duration::from_secs(idle.duration_secs))
        .await?;
    let last = instruments.sample().await?;
    let after = instruments.transactions().await?;
    // NOTE: both counts are lower bounds (crate::database), so the larger holds.
    let counted = delta(before, after)?.map(|count| count.max(witnessed.unwrap_or(0)));
    let elapsed = started.elapsed().as_secs_f64();
    let per_second = |value: f64| value / elapsed;
    Ok(IdleReport {
        duration_secs: elapsed,
        cpu_percent: cpu_between(first, last).map(|cpu| per_second(cpu.as_secs_f64()) * 100.0),
        max_cpu_percent: idle.max_cpu_percent,
        transactions_per_second: counted.map(|count| per_second(crate::quantile::exact_f64(count))),
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

/// An instant 50 ms from now, when a load phase can start on time.
pub(crate) fn soon() -> Instant {
    Instant::now()
        .checked_add(Duration::from_millis(50))
        .unwrap_or_else(Instant::now)
}

/// One load phase whose schedule starts at `start`, with its warm-up, its
/// measured window and its settle.
pub(crate) async fn load_phase(
    plan: &Plan,
    endpoints: &Endpoints,
    client: &reqwest::Client,
    credentials: &Credentials,
    instruments: &Instruments,
    start: Instant,
) -> Result<LoadReport, RunError> {
    let load = &plan.load;
    let before = instruments.transactions().await?;
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
    let driven = load::drive(plan, endpoints, client, credentials, start).await;
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
        stall_ratio: Some(load.stall_ratio),
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
    let old = Baselines::read(&options.baseline)?;
    let base = Endpoints::new(
        &options.plan,
        server_url(&options.base_url)?,
        options.mllp_address.as_deref(),
    )?;
    let (credentials, client) = client(&options.plan)?;
    let instruments = Instruments::open(&options.target, options.database_url.as_ref()).await?;
    let started = now();
    let plan = &options.plan;
    let idle = idle_phase(plan, &instruments).await?;
    let load = load_phase(plan, &base, &client, &credentials, &instruments, soon()).await?;
    let mut calibration = Vec::new();
    if options.update {
        for _ in 1..options.runs.max(2) {
            calibration
                .push(load_phase(plan, &base, &client, &credentials, &instruments, soon()).await?);
        }
    }
    let mut report = Report {
        schema: SCHEMA,
        tool: env!("CARGO_PKG_VERSION").to_owned(),
        started,
        finished: now(),
        host: Fingerprint::detect(),
        target: TargetInfo {
            process: options.target.describe(),
            base_url: options.base_url.redacted(),
            database: instruments.database.as_ref().map(|d| d.name().to_owned()),
            mllp: base.mllp_address().map(str::to_owned),
        },
        plan: identity(plan),
        idle,
        load,
        calibration,
        confirmations: Vec::new(),
    };
    while !options.update && needs_confirmation(&report, old.as_ref())? {
        report
            .confirmations
            .push(load_phase(plan, &base, &client, &credentials, &instruments, soon()).await?);
        report.finished = now();
    }
    conclude(report, old.as_ref(), options)
}

/// Whether one more fresh load phase could decide a breach of `report`.
///
/// It could while a load metric breached in some run but not in most of
/// three, and fewer than two confirmations ran. An update measures its own
/// phases and confirms nothing.
///
/// # Errors
///
/// [`RunError::Report`] when a histogram of the report does not decode.
pub fn needs_confirmation(report: &Report, old: Option<&Baselines>) -> Result<bool, RunError> {
    let confirmed = u32::try_from(report.confirmations.len()).unwrap_or(u32::MAX);
    if confirmed.saturating_add(1) >= crate::compare::RUNS {
        return Ok(false);
    }
    let verdict = judge_file(report, old, false)?;
    Ok(verdict.checks.iter().any(crate::compare::Check::undecided))
}

/// What makes runs of `plan` comparable.
#[must_use]
pub fn identity(plan: &Plan) -> PlanIdentity {
    PlanIdentity {
        rate: plan.load.rate,
        warmup_secs: plan.load.warmup_secs,
        duration_secs: plan.load.duration_secs,
        requests: u64::try_from(plan.requests.len().saturating_add(plan.messages.len()))
            .unwrap_or(u64::MAX),
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
    old: Option<&Baselines>,
    options: &RunOptions,
) -> Result<RunResult, RunError> {
    let verdict = judge_file(&report, old, options.update)?;
    let report_path = crate::report::write(&report, &options.out_dir, &stamp())?;
    let mut text = summary::markdown(&report, &verdict);
    let mut changes = None;
    if options.update {
        if verdict.outcome == Outcome::Pass {
            let phases = std::iter::once(&report.load)
                .chain(report.calibration.iter())
                .map(metric::of)
                .collect::<Result<Vec<_>, _>>()?;
            let (metrics, noise) = baseline::across(&phases);
            let written = now();
            let entry = old.and_then(|file| file.entry(baseline::Shape::of(&report.host)));
            let (baseline, done) = baseline::updated(
                entry,
                &Update {
                    metrics: &metrics,
                    noise,
                    plan: &report.plan,
                    host: &report.host,
                    tool: &report.tool,
                    written: &written,
                },
            );
            let file = old.cloned().unwrap_or(Baselines {
                schema: baseline::SCHEMA,
                entries: Vec::new(),
            });
            let model = baseline.shape().to_string();
            file.with(baseline).write(&options.baseline)?;
            text.push_str(&changes_markdown(&options.baseline, &model, &done));
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

fn changes_markdown(path: &Path, model: &str, changes: &[Change]) -> String {
    let mut text = format!(
        "### Baseline\n\nThe entry for the host {model}, the median of the update's load \
         phases, written to `{}`:\n\n",
        path.display()
    );
    for change in changes {
        let item = match change {
            Change::Added(metric, value) => format!("{metric} added at {value:.3}"),
            Change::Tightened { metric, from, to } => {
                format!("{metric} tightened from {from:.3} to {to:.3}")
            }
            Change::Kept(metric, value) => format!("{metric} kept at {value:.3}"),
            Change::Rewritten => "written afresh for the new plan".to_owned(),
            Change::Remeasured => {
                "measured again across runs, replacing the entry measured within one run".to_owned()
            }
        };
        text.push_str("- ");
        text.push_str(&item);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Baseline, Shape};
    use crate::report::fixture;

    fn options(dir: &Path) -> RunOptions {
        RunOptions {
            plan: Plan::parse(
                "[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/\"\nstatus = 200\n",
            )
            .unwrap(),
            target: Target::Pid(1),
            base_url: SecretUrl::new("http://127.0.0.1:8080".to_owned()),
            mllp_address: None,
            database_url: None,
            baseline: dir.join("perf/baseline.toml"),
            out_dir: dir.join("reports"),
            update: true,
            runs: 2,
        }
    }

    fn on_model(latency_ms: u64, model: &str) -> Report {
        let mut report = fixture::report();
        report.host.cpu_model = Some(model.to_owned());
        report.load = fixture::load(latency_ms, 250);
        report.calibration = vec![fixture::load(latency_ms, 250)];
        report
    }

    #[test]
    fn an_update_writes_the_entry_of_the_runners_model_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        conclude(on_model(20, "EPYC 7763"), None, &options).unwrap();
        let first = Baselines::read(&options.baseline).unwrap().unwrap();
        let slow = first
            .entry(Shape::new(Some("EPYC 7763"), None))
            .unwrap()
            .clone();

        let result = conclude(on_model(10, "EPYC 9V45"), Some(&first), &options).unwrap();
        assert!(
            result
                .summary
                .contains("The entry for the host `EPYC 9V45`"),
            "{}",
            result.summary
        );
        let both = Baselines::read(&options.baseline).unwrap().unwrap();
        assert_eq!(2, both.entries.len());
        assert_eq!(
            Some(&slow),
            both.entry(Shape::new(Some("EPYC 7763"), None)),
            "untouched"
        );

        // A faster run of the slow model tightens its entry alone.
        let fast = both
            .entry(Shape::new(Some("EPYC 9V45"), None))
            .unwrap()
            .clone();
        conclude(on_model(15, "EPYC 7763"), Some(&both), &options).unwrap();
        let after = Baselines::read(&options.baseline).unwrap().unwrap();
        assert_eq!(
            Some(&fast),
            after.entry(Shape::new(Some("EPYC 9V45"), None)),
            "untouched"
        );
        let p50 = |entry: &Baseline| {
            entry
                .metrics
                .iter()
                .find(|(metric, _)| metric.to_string() == "read.p50_ms")
                .map(|(_, value)| *value)
                .unwrap()
        };
        assert!(
            p50(after.entry(Shape::new(Some("EPYC 7763"), None)).unwrap()) < p50(&slow),
            "the slow model's entry tightened"
        );
    }

    #[test]
    fn a_breach_asks_for_fresh_runs_until_most_of_three_agree() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        conclude(on_model(10, "EPYC 7763"), None, &options).unwrap();
        let file = Baselines::read(&options.baseline).unwrap().unwrap();
        let quiet = on_model(10, "EPYC 7763");
        assert!(!needs_confirmation(&quiet, Some(&file)).unwrap());
        let mut slower = on_model(20, "EPYC 7763");
        assert!(
            needs_confirmation(&slower, Some(&file)).unwrap(),
            "a first breach"
        );
        slower.confirmations.push(fixture::load(20, 250));
        assert!(
            !needs_confirmation(&slower, Some(&file)).unwrap(),
            "two of two decide it"
        );
        let mut stalled = on_model(20, "EPYC 7763");
        stalled.confirmations.push(fixture::load(10, 250));
        assert!(
            needs_confirmation(&stalled, Some(&file)).unwrap(),
            "the third decides"
        );
        stalled.confirmations.push(fixture::load(10, 250));
        assert!(
            !needs_confirmation(&stalled, Some(&file)).unwrap(),
            "at most two"
        );
        let unknown = on_model(20, "EPYC 9V74");
        assert!(
            !needs_confirmation(&unknown, Some(&file)).unwrap(),
            "no entry, nothing to confirm"
        );
    }
}
