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
//! one per arm, each after its own warm-up. The first pair runs the base
//! first, the next the head first, and so on (AB, BA, AB), so a drift over
//! the job reaches both arms alike and the order is recorded with each
//! pair.
//!
//! Each load metric is judged as the head's median over its phases against
//! the base's. The limit is the larger of the plan's `[tolerance]` and the
//! noise floor, the worse of the two arms' spread ([`crate::baseline::across`]).
//! The metric fails when that median breaches the limit and the head
//! breaches it in most of the pairs, so a stall in one phase does not fail
//! the gate and a real regression, which repeats, does. A phase that
//! stalled, lagged or was not steady makes the run one that cannot be
//! judged, exit 2, as in a run against a baseline; a breach outranks it. No
//! specification governs the mode: our own design (#278).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use ferrohealth_secret::secret_url::SecretUrl;
use serde::{Deserialize, Serialize};

use crate::baseline::{Tolerances, across};
use crate::compare::{Check, Findings, Limit, Verdict, add_failures, schedule_problems};
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

/// The schema of the A/B report this version writes and reads.
pub const SCHEMA: u32 = 1;

/// The pairs of load phases a run measures unless told otherwise.
pub const DEFAULT_PAIRS: u32 = 3;

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
    /// The pairs of load phases, at least two.
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
        let (value, parse) = crate::report::read_json(path)?;
        let report: Self = serde_json::from_value(value).map_err(parse)?;
        if report.schema == SCHEMA {
            Ok(report)
        } else {
            Err(ReportError::Schema(report.schema))
        }
    }
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

/// The two servers, each with its URL and its instruments.
#[derive(Debug)]
struct Arms {
    base: (Endpoints, Instruments),
    head: (Endpoints, Instruments),
}

impl Arms {
    fn of(&self, arm: Arm) -> (&Endpoints, &Instruments) {
        let (url, instruments) = match arm {
            Arm::Base => &self.base,
            Arm::Head => &self.head,
        };
        (url, instruments)
    }
}

/// The endpoints and the instruments of `server`.
async fn arm(plan: &Plan, server: &Server) -> Result<(Endpoints, Instruments), RunError> {
    let url = server_url(&server.url)?;
    let endpoints = Endpoints::new(plan, url, server.mllp_address.as_deref())?;
    let instruments = Instruments::open(&server.target, server.database_url.as_ref()).await?;
    Ok((endpoints, instruments))
}

/// Carries out one A/B run: the head's idle phase, then `pairs` pairs of
/// load phases in alternating order, the judgement and the report files.
///
/// # Errors
///
/// A [`RunError`] when the run cannot be carried out: a URL that is not a
/// URL, a server or database that cannot be read, a load phase that fails,
/// or a report that cannot be written.
pub async fn run(options: &AbOptions) -> Result<AbResult, RunError> {
    let plan = &options.plan;
    let (credentials, client) = client(plan)?;
    let arms = Arms {
        base: arm(plan, &options.base).await?,
        head: arm(plan, &options.head).await?,
    };
    let started = now();
    let idle = idle_phase(plan, &arms.head.1).await?;
    let mut pairs = Vec::new();
    for index in 0..options.pairs.max(2) {
        let first = Arm::first_of(index);
        let (url, instruments) = arms.of(first);
        let one = load_phase(plan, url, &client, &credentials, instruments).await?;
        let (url, instruments) = arms.of(first.other());
        let two = load_phase(plan, url, &client, &credentials, instruments).await?;
        let (base, head) = match first {
            Arm::Base => (one, two),
            Arm::Head => (two, one),
        };
        pairs.push(Pair { first, base, head });
    }
    let describe =
        |server: &Server, (endpoints, instruments): &(Endpoints, Instruments)| TargetInfo {
            process: server.target.describe(),
            base_url: server.url.redacted(),
            database: instruments.database.as_ref().map(|d| d.name().to_owned()),
            mllp: endpoints.mllp_address().map(str::to_owned),
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
        verdict: None,
    };
    let verdict = judge(&report)?;
    report.verdict = Some(verdict.clone());
    let report_path = crate::report::write(&report, &options.out_dir, &stamp())?;
    let summary = summary::ab_markdown(&report, &verdict);
    Ok(AbResult {
        report,
        verdict,
        report_path,
        summary,
    })
}

/// The metrics of every phase of `arm` that did not stall, and of each
/// pair, `None` for an arm whose phase stalled.
type Measured = (
    Vec<BTreeMap<Metric, f64>>,
    Vec<Option<BTreeMap<Metric, f64>>>,
);

fn measured(report: &AbReport, arm: Arm) -> Result<Measured, ReportError> {
    let mut phases = Vec::new();
    let mut by_pair = Vec::new();
    for pair in &report.pairs {
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
    phase_problems(report, &mut checks, &mut problems)?;
    let (base_phases, base_pairs) = measured(report, Arm::Base)?;
    let (head_phases, head_pairs) = measured(report, Arm::Head)?;
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

/// The problems of every phase, by arm and pair, and the head's saturation:
/// a head that fell behind its schedule where the base kept it fails, and a
/// base that fell behind cannot be compared with.
fn phase_problems(
    report: &AbReport,
    checks: &mut Vec<Check>,
    problems: &mut Vec<String>,
) -> Result<(), ReportError> {
    let mut saturated = BTreeMap::from([(Arm::Base, 0_u32), (Arm::Head, 0_u32)]);
    for (index, pair) in report.pairs.iter().enumerate() {
        for arm in [pair.first, pair.first.other()] {
            let phase = pair.of(arm);
            let mut found = Vec::new();
            schedule_problems(phase, &mut found)?;
            if phase.stalled() {
                found.push(format!(
                    "the runner stalled: every route's p99 is over {} times its p50 at once, so \
                     the phase is not judged; re-run the job on a fresh runner",
                    phase.stall_ratio.unwrap_or(0.0)
                ));
            }
            if phase.saturated
                && let Some(count) = saturated.get_mut(&arm)
            {
                *count = count.saturating_add(1);
            }
            problems.extend(
                found
                    .into_iter()
                    .map(|problem| format!("the {arm}'s phase of pair {}: {problem}", index + 1)),
            );
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
    Ok(())
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
