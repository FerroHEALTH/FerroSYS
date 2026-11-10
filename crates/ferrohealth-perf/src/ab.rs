// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The A/B mode: the head of a change judged against its base, both
//! servers measured on one host in one job.
//!
//! Hosts that report the same CPU model and memory still differ by 25 to
//! 45 % in latency and CPU per request, so a baseline stored on one machine
//! cannot judge a run on another. The product starts the base and the head
//! side by side, and the run measures both: the idle phase against the
//! head, held to the plan's absolute limits, then pairs of load phases,
//! one per arm, each after its own warm-up.
//!
//! The two phases of a pair run at once, each arm at the plan's rate, with
//! the arrivals of one arm half an interval after the other's, so the
//! servers take turns and the host's state at every moment reaches both.
//! Phases run one after the other did not: on GitHub's AMD EPYC 9V74
//! runners the latency of unchanged code moved by up to a third from one
//! phase to the next, and four phases an arm put the head up to 37 % off
//! its base (#287). The base leads in the first pair, the head in the next,
//! and so on, with an even count of pairs (#283); the arm that leads is
//! recorded with each pair as its `first`.
//!
//! Each load metric is judged as the head's median over its phases against
//! the base's. The limit is the larger of the plan's `[tolerance]` and the
//! noise floor, the worse of the two arms' spread ([`crate::baseline::across`]).
//! The metric fails when that median breaches the limit and the head
//! breaches it in most of the pairs, so a stall in one phase does not fail
//! the gate and a real regression, which repeats, does. A pair whose phase
//! stalled or lagged, or whose comparison moved within the window (the
//! head's ratio of halves over the base's), cannot be judged; the run
//! measures up to [`EXTRA_PAIRS`] more in its place, and without the pairs
//! it wanted it exits 2, as in a run against a baseline; a breach outranks
//! it.
//!
//! [`run_with`] fills each arm's copy of the plan with that arm's
//! [`Values`], so a request can name an id each arm's server assigned on
//! its own (#286); the report records the values of both. No
//! specification governs the mode: our own design (#278).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferrohealth_secret::secret_url::SecretUrl;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;

use crate::baseline::{Tolerances, across};
use crate::compare::{Check, Findings, Limit, Verdict, add_failures, lag_problem, unsteady};
use crate::fingerprint::Fingerprint;
use crate::load::Endpoints;
use crate::metric::{self, Metric};
use crate::plan::Plan;
use crate::process::Target;
use crate::report::{IdleReport, LoadReport, PlanIdentity, ReportError, TargetInfo};
use crate::run::{
    Instruments, RunError, client, identity, idle_phase, load_phase, now, server_url, stamp,
};
use crate::summary;
use crate::values::{Values, ValuesError};

/// The schema of the A/B report this version writes and reads.
pub const SCHEMA: u32 = 1;

/// The pairs of load phases a run measures unless told otherwise, an even
/// count.
pub const DEFAULT_PAIRS: u32 = 4;

/// The most pairs a run measures beyond the count it was given, each in
/// place of a pair that could not be judged.
pub const EXTRA_PAIRS: u32 = 2;

/// How long before the leading arm's schedule starts a pair is set up, so
/// both arms read their databases' counts first.
const PHASE_LEAD: Duration = Duration::from_millis(250);

/// One of the two servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    /// The server built from the change's base, `main`.
    Base,
    /// The server built from the change.
    Head,
}

impl Arm {
    /// The arm a pair measures first: the base in the first pair, then the
    /// head, alternating.
    #[must_use]
    pub fn first_of(pair: u32) -> Self {
        if pair.is_multiple_of(2) {
            Self::Base
        } else {
            Self::Head
        }
    }

    /// The other arm.
    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::Base => Self::Head,
            Self::Head => Self::Base,
        }
    }
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Head => "head",
        })
    }
}

/// One server a run measures.
#[derive(Debug, Clone)]
pub struct Server {
    /// Its process or cgroup.
    pub target: Target,
    /// Its base URL.
    pub url: SecretUrl,
    /// Its database, when its transactions are counted.
    pub database_url: Option<SecretUrl>,
    /// Its MLLP listener, `host:port`, in place of the plan's `[mllp]
    /// address`.
    pub mllp_address: Option<String>,
}

/// What an A/B run is given.
#[derive(Debug, Clone)]
pub struct AbOptions {
    /// The plan, the head's.
    pub plan: Plan,
    /// The server built from the base.
    pub base: Server,
    /// The server built from the change.
    pub head: Server,
    /// The pairs of load phases, an even count of at least two; [`run`]
    /// measures one pair more for an odd count, and two for a count under
    /// two.
    pub pairs: u32,
    /// The directory the reports are written to.
    pub out_dir: PathBuf,
}

/// One pair of load phases, one per arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    /// The arm measured first.
    pub first: Arm,
    /// The base's phase.
    pub base: LoadReport,
    /// The head's phase.
    pub head: LoadReport,
}

impl Pair {
    /// The phase of `arm`.
    #[must_use]
    pub fn of(&self, arm: Arm) -> &LoadReport {
        match arm {
            Arm::Base => &self.base,
            Arm::Head => &self.head,
        }
    }
}

/// One A/B run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbReport {
    /// The report schema, [`SCHEMA`].
    pub schema: u32,
    /// The version of `ferrohealth-perf` that wrote it.
    pub tool: String,
    /// When the run started, RFC 3339 in UTC.
    pub started: String,
    /// When the run finished, RFC 3339 in UTC.
    pub finished: String,
    /// The host both arms were measured on.
    pub host: Fingerprint,
    /// The plan the run followed.
    pub plan: PlanIdentity,
    /// The tolerances the plan names.
    pub tolerance: Tolerances,
    /// The base server.
    pub base: TargetInfo,
    /// The head server.
    pub head: TargetInfo,
    /// The head's idle phase.
    pub idle: IdleReport,
    /// The pairs of load phases, in the order they ran.
    pub pairs: Vec<Pair>,
    /// The judgeable pairs the run set out to measure, half led by each
    /// arm; zero in a report written before 0.9.0, which judges every pair.
    #[serde(default)]
    pub wanted: u32,
    /// The judgement the run wrote, for a reader; [`judge`] derives it
    /// again from the phases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

impl AbReport {
    /// Reads an A/B report from `path`.
    ///
    /// # Errors
    ///
    /// [`ReportError::Io`], [`ReportError::Parse`] for a file that is not an
    /// A/B report, or [`ReportError::Schema`] for a newer schema.
    pub fn read(path: &Path) -> Result<Self, ReportError> {
        let (mut value, parse) = crate::report::read_json(path)?;
        if let Some(object) = value.as_object_mut() {
            object.remove(VALUES);
        }
        let report: Self = serde_json::from_value(value).map_err(parse)?;
        if report.schema == SCHEMA {
            Ok(report)
        } else {
            Err(ReportError::Schema(report.schema))
        }
    }
}

/// The plan values of each arm, [`crate::values`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmValues {
    /// The base's.
    #[serde(default)]
    pub base: Values,
    /// The head's.
    #[serde(default)]
    pub head: Values,
}

impl ArmValues {
    /// The values of `arm`.
    #[must_use]
    pub fn of(&self, arm: Arm) -> &Values {
        match arm {
            Arm::Base => &self.base,
            Arm::Head => &self.head,
        }
    }

    /// Whether neither arm has a value.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.head.is_empty()
    }
}

/// An A/B run with plan values that could not be carried out.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AbError {
    /// An arm's values cannot fill the plan; no load was sent.
    #[error("the {arm}'s plan values are refused")]
    Values {
        /// The arm.
        arm: Arm,
        /// Why.
        #[source]
        source: ValuesError,
    },
    /// The run failed.
    #[error(transparent)]
    Run(#[from] RunError),
}

/// What an A/B run produced.
#[derive(Debug, Clone)]
pub struct AbResult {
    /// The report, its verdict included.
    pub report: AbReport,
    /// Its judgement.
    pub verdict: Verdict,
    /// The dated report file.
    pub report_path: PathBuf,
    /// The Markdown summary.
    pub summary: String,
}

/// One server's filled plan, its endpoints and its instruments.
#[derive(Debug)]
struct Side {
    plan: Plan,
    endpoints: Endpoints,
    instruments: Instruments,
}

/// The two servers.
#[derive(Debug)]
struct Arms {
    base: Side,
    head: Side,
}

impl Arms {
    fn of(&self, arm: Arm) -> &Side {
        match arm {
            Arm::Base => &self.base,
            Arm::Head => &self.head,
        }
    }
}

/// The side of `server` over `plan`, its message files filled with
/// `values` when there are any.
async fn side(plan: Plan, server: &Server, values: Option<&Values>) -> Result<Side, RunError> {
    let url = server_url(&server.url)?;
    let mllp = server.mllp_address.as_deref();
    let endpoints = match values {
        Some(values) => Endpoints::filled(&plan, url, mllp, values)?,
        None => Endpoints::new(&plan, url, mllp)?,
    };
    let instruments = Instruments::open(&server.target, server.database_url.as_ref()).await?;
    Ok(Side {
        plan,
        endpoints,
        instruments,
    })
}

/// The report as written: the run, and the plan values of each arm when
/// there are any.
#[derive(Serialize)]
struct Written<'a> {
    #[serde(flatten)]
    report: &'a AbReport,
    #[serde(skip_serializing_if = "ArmValues::is_empty")]
    values: &'a ArmValues,
}

/// The key of the written report that holds the plan values.
const VALUES: &str = "values";

/// The plan values the A/B report at `path` records, none for a run that
/// had none.
///
/// # Errors
///
/// [`ReportError::Io`], or [`ReportError::Parse`] for values that do not
/// read.
pub fn recorded_values(path: &Path) -> Result<ArmValues, ReportError> {
    let (value, parse) = crate::report::read_json(path)?;
    match value.get(VALUES) {
        Some(values) => serde_json::from_value(values.clone()).map_err(parse),
        None => Ok(ArmValues::default()),
    }
}

/// Carries out one A/B run: the head's idle phase, then `pairs` pairs of
/// load phases in alternating order, raised to an even count of at least
/// two, the judgement and the report files.
///
/// # Errors
///
/// A [`RunError`] when the run cannot be carried out: a URL that is not a
/// URL, a server or database that cannot be read, a load phase that fails,
/// or a report that cannot be written.
pub async fn run(options: &AbOptions) -> Result<AbResult, RunError> {
    let plans = (options.plan.clone(), options.plan.clone());
    carry_out(options, plans, None).await
}

/// Carries out the A/B run of [`run`], each arm's plan filled with its
/// `values`.
///
/// Every placeholder of the requests and the message files needs a value
/// for both arms, and every value a placeholder.
///
/// # Errors
///
/// [`AbError::Values`] before any phase when an arm's values cannot fill
/// the plan, naming the arm, and [`AbError::Run`] for every error of
/// [`run`].
pub async fn run_with(options: &AbOptions, values: &ArmValues) -> Result<AbResult, AbError> {
    let fill = |arm| {
        values
            .of(arm)
            .fill_plan(&options.plan)
            .map_err(|source| AbError::Values { arm, source })
    };
    let plans = (fill(Arm::Base)?, fill(Arm::Head)?);
    Ok(carry_out(options, plans, Some(values)).await?)
}

async fn carry_out(
    options: &AbOptions,
    (base_plan, head_plan): (Plan, Plan),
    values: Option<&ArmValues>,
) -> Result<AbResult, RunError> {
    let plan = &options.plan;
    let (credentials, client) = client(plan)?;
    let arms = Arms {
        base: side(base_plan, &options.base, values.map(|v| &v.base)).await?,
        head: side(head_plan, &options.head, values.map(|v| &v.head)).await?,
    };
    let started = now();
    let idle = idle_phase(plan, &arms.head.instruments).await?;
    let mut pairs: Vec<Pair> = Vec::new();
    let lag = Duration::try_from_secs_f64(0.5 / plan.load.rate).unwrap_or(Duration::ZERO);
    let wanted = even_pairs(options.pairs);
    let half = wanted.div_euclid(2);
    let mut kept = BTreeMap::from([(Arm::Base, 0_u32), (Arm::Head, 0_u32)]);
    let count = |kept: &BTreeMap<Arm, u32>, arm| kept.get(&arm).copied().unwrap_or(0);
    for index in 0..wanted.saturating_add(EXTRA_PAIRS) {
        let first = if index < wanted {
            Arm::first_of(index)
        } else if count(&kept, Arm::Base) < half {
            Arm::Base
        } else if count(&kept, Arm::Head) < half {
            Arm::Head
        } else {
            break;
        };
        let lead = Instant::now()
            .checked_add(PHASE_LEAD)
            .unwrap_or_else(Instant::now);
        let follow = lead.checked_add(lag).unwrap_or(lead);
        let (one, two) = tokio::try_join!(
            phase(arms.of(first), &client, &credentials, lead),
            phase(arms.of(first.other()), &client, &credentials, follow),
        )?;
        let (base, head) = match first {
            Arm::Base => (one, two),
            Arm::Head => (two, one),
        };
        let pair = Pair { first, base, head };
        if pair_problems(&pair)?.is_empty()
            && let Some(judgeable) = kept.get_mut(&first)
        {
            *judgeable = judgeable.saturating_add(1);
        }
        pairs.push(pair);
    }
    let describe = |server: &Server, side: &Side| TargetInfo {
        process: server.target.describe(),
        base_url: server.url.redacted(),
        database: side
            .instruments
            .database
            .as_ref()
            .map(|d| d.name().to_owned()),
        mllp: side.endpoints.mllp_address().map(str::to_owned),
    };
    let mut report = AbReport {
        schema: SCHEMA,
        tool: env!("CARGO_PKG_VERSION").to_owned(),
        started,
        finished: now(),
        host: Fingerprint::detect(),
        plan: identity(plan),
        tolerance: plan.tolerance,
        base: describe(&options.base, &arms.base),
        head: describe(&options.head, &arms.head),
        idle,
        pairs,
        wanted,
        verdict: None,
    };
    let verdict = judge(&report)?;
    report.verdict = Some(verdict.clone());
    let none = ArmValues::default();
    let values = values.unwrap_or(&none);
    let written = Written {
        report: &report,
        values,
    };
    let report_path = crate::report::write(&written, &options.out_dir, &stamp())?;
    let summary = summary::ab_markdown_with_values(&report, &verdict, values);
    Ok(AbResult {
        report,
        verdict,
        report_path,
        summary,
    })
}

/// One load phase of `side`.
async fn phase(
    side: &Side,
    client: &reqwest::Client,
    credentials: &crate::credentials::Credentials,
    start: Instant,
) -> Result<LoadReport, RunError> {
    load_phase(
        &side.plan,
        &side.endpoints,
        client,
        credentials,
        &side.instruments,
        start,
    )
    .await
}

/// `pairs` raised to an even count of at least two, so the head runs first
/// in as many pairs as the base.
fn even_pairs(pairs: u32) -> u32 {
    pairs.max(2).saturating_add(1) & !1
}

/// The metrics of every phase of `arm` that did not stall, and of each
/// pair, `None` for an arm whose phase stalled.
type Measured = (
    Vec<BTreeMap<Metric, f64>>,
    Vec<Option<BTreeMap<Metric, f64>>>,
);

fn measured(pairs: &[&Pair], arm: Arm) -> Result<Measured, ReportError> {
    let mut phases = Vec::new();
    let mut by_pair = Vec::new();
    for pair in pairs {
        let phase = pair.of(arm);
        if phase.stalled() {
            by_pair.push(None);
        } else {
            let metrics = metric::of(phase)?;
            phases.push(metrics.clone());
            by_pair.push(Some(metrics));
        }
    }
    Ok((phases, by_pair))
}

/// Judges `report`: the head's idle limits, the requests the head answered
/// other than expected, and every load metric of the head over the base.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram of the report does not
/// decode.
pub fn judge(report: &AbReport) -> Result<Verdict, ReportError> {
    let Findings {
        mut checks,
        mut problems,
        mut warnings,
    } = Findings::idle(&report.idle);
    let judged = judged_pairs(report, &mut problems, &mut warnings)?;
    saturation(report, &mut checks, &mut problems);
    let (base_phases, base_pairs) = measured(&judged, Arm::Base)?;
    let (head_phases, head_pairs) = measured(&judged, Arm::Head)?;
    let (base, base_noise) = across(&base_phases);
    let (head, head_noise) = across(&head_phases);
    for (metric, measured) in &head {
        let Some(reference) = base.get(metric).copied() else {
            warnings.push(format!(
                "{metric} was not measured on the base, so it is not judged"
            ));
            continue;
        };
        let noise = |floors: &BTreeMap<Metric, f64>| floors.get(metric).copied().unwrap_or(1.0);
        let floor = noise(&base_noise).max(noise(&head_noise));
        let tolerance = report.tolerance.of(metric.kind());
        let limit = Limit::new(metric.kind(), tolerance, floor);
        let (mut runs, mut breached) = (0_u32, 0_u32);
        for (b, h) in base_pairs.iter().zip(&head_pairs) {
            let at = |phase: &Option<BTreeMap<Metric, f64>>| phase.as_ref()?.get(metric).copied();
            if let (Some(b), Some(h)) = (at(b), at(h)) {
                runs = runs.saturating_add(1);
                breached = breached.saturating_add(u32::from(limit.breached(b, h)));
            }
        }
        let passed = !(limit.breached(reference, *measured) && breached.saturating_mul(2) > runs);
        checks.push(Check {
            name: metric.to_string(),
            measured: *measured,
            reference,
            threshold: limit.threshold(reference),
            rule: format!(
                "{}; breached in {breached} of {runs} pairs",
                limit.rule("the base")
            ),
            passed,
            runs,
            breached,
            ratio: (reference > 0.0).then(|| measured / reference),
            tolerance: Some(tolerance),
            floor: Some(floor),
        });
    }
    for metric in base.keys().filter(|metric| !head.contains_key(*metric)) {
        warnings.push(format!(
            "{metric} was measured on the base only, so it is not judged"
        ));
    }
    let failures = failures_of(report, Arm::Head);
    for (group, kinds) in failures_of(report, Arm::Base) {
        for (kind, count) in kinds {
            warnings.push(format!(
                "the base answered {count} {group} request(s) other than expected ({kind})"
            ));
        }
    }
    Ok(Findings {
        checks,
        problems,
        warnings,
    }
    .verdict(failures))
}

/// Why `pair` cannot be judged, each problem with the arm whose phase it
/// is, or none for the comparison of the two: a generator that fell behind,
/// a runner that stalled, a comparison that moved within the window.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram of the pair does not decode.
fn pair_problems(pair: &Pair) -> Result<Vec<(Option<Arm>, String)>, ReportError> {
    let mut found = Vec::new();
    for arm in [pair.first, pair.first.other()] {
        let phase = pair.of(arm);
        found.extend(lag_problem(phase).map(|problem| (Some(arm), problem)));
        if phase.stalled() {
            found.push((
                Some(arm),
                format!(
                    "the runner stalled: every route's p99 is over {} times its p50 at once, so \
                     the phase is not judged; re-run the job on a fresh runner",
                    phase.stall_ratio.unwrap_or(0.0)
                ),
            ));
        }
    }
    // NOTE: no specification governs this: our own design (#357); a drift of the host reaches both arms at once and cancels in the head over the base.
    let steadiness = pair.head.steadiness()?.over(&pair.base.steadiness()?);
    found.extend(
        unsteady(
            &steadiness,
            "the comparison is not steady: the head's second half over its first, over the \
             base's, on p50 and p95,",
        )
        .map(|problem| (None, problem)),
    );
    Ok(found)
}

/// The pairs the metrics are judged on. When the run holds the pairs it
/// wanted that can be judged, half led by each arm, those are judged and
/// each other pair is left out with a warning; otherwise every pair is
/// judged and each problem makes the run one that cannot be judged.
fn judged_pairs<'a>(
    report: &'a AbReport,
    problems: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<Vec<&'a Pair>, ReportError> {
    let mut found = Vec::new();
    let mut kept = BTreeMap::from([(Arm::Base, 0_u32), (Arm::Head, 0_u32)]);
    for (index, pair) in report.pairs.iter().enumerate() {
        let problems = pair_problems(pair)?;
        if problems.is_empty()
            && let Some(count) = kept.get_mut(&pair.first)
        {
            *count = count.saturating_add(1);
        }
        found.push((index, pair, problems));
    }
    let half = report.wanted.div_euclid(2);
    let replaced = report.wanted > 0 && kept.values().all(|count| *count >= half);
    let mut judged = Vec::new();
    for (index, pair, found) in found {
        let number = index.saturating_add(1);
        let lines = found.into_iter().map(|(arm, problem)| match arm {
            Some(arm) => format!("the {arm}'s phase of pair {number}: {problem}"),
            None => format!("pair {number}: {problem}"),
        });
        if !replaced {
            problems.extend(lines);
            judged.push(pair);
        } else if lines.len() == 0 {
            judged.push(pair);
        } else {
            warnings.extend(lines.map(|line| {
                format!("{line}; the pair is left out and a later pair led by the {} judged in its place", pair.first)
            }));
        }
    }
    Ok(judged)
}

/// The arms' saturation over every pair: a head that fell behind its
/// schedule where the base kept it fails, and a base that fell behind
/// cannot be compared with.
fn saturation(report: &AbReport, checks: &mut Vec<Check>, problems: &mut Vec<String>) {
    let mut saturated = BTreeMap::from([(Arm::Base, 0_u32), (Arm::Head, 0_u32)]);
    for pair in &report.pairs {
        for arm in [Arm::Base, Arm::Head] {
            if pair.of(arm).saturated
                && let Some(count) = saturated.get_mut(&arm)
            {
                *count = count.saturating_add(1);
            }
        }
    }
    let count = |arm| saturated.get(&arm).copied().unwrap_or(0);
    let pairs = u32::try_from(report.pairs.len()).unwrap_or(u32::MAX);
    if count(Arm::Base) > 0 {
        problems.push(format!(
            "the base fell behind the schedule in {} of {pairs} phases: the rate is too high \
             for this runner",
            count(Arm::Base)
        ));
    } else if count(Arm::Head) > 0 {
        checks.push(Check {
            name: "load.saturation".to_owned(),
            measured: f64::from(count(Arm::Head)),
            reference: 0.0,
            threshold: 0.0,
            rule: "an arrival waited past the saturation limit on the head and never on the base"
                .to_owned(),
            passed: false,
            runs: pairs,
            breached: count(Arm::Head),
            ratio: None,
            tolerance: None,
            floor: None,
        });
    }
}

/// The requests `arm` answered other than expected over all its phases, by
/// group and kind.
fn failures_of(report: &AbReport, arm: Arm) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut failures = BTreeMap::new();
    for pair in &report.pairs {
        add_failures(&mut failures, &pair.of(arm).failures);
    }
    failures
}

#[cfg(test)]
mod tests;
