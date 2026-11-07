// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The judgement of a run: the idle limits, the failed requests, and every
//! load metric against the baseline entry of the runner's host shape.
//!
//! A metric breaches when it is worse than the baseline by more than the
//! larger of its tolerance and its noise floor across the update's load
//! phases (Kalibera and Jones,
//! "Rigorous Benchmarking in Reasonable Time", ISMM 2013, on measuring the
//! noise before judging a difference); the floor follows Veredictum's
//! `perf_result::compare` (Apache-2.0, credited in `NOTICE`). A run that
//! cannot be judged (no baseline, no entry for the runner's host shape,
//! another plan, a metric the baseline holds
//! and the run lacks, a generator that fell behind, an unsteady window) is
//! invalid. A breach of a load metric is confirmed on up to two fresh load
//! phases and fails only when the metric breaches in most of the runs, so a
//! one-off stall or a noisy runner does not fail the gate and a real
//! regression, which repeats, does. A phase where every route's p99 is far
//! above its p50 at once marks a stalled runner and is not judged. The exit
//! code is 0 for a pass, 1 for a breach and 2 for an invalid run or a
//! harness error; a breach outranks an invalid run.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::baseline::{Baseline, Baselines, Shape};
use crate::metric::{Kind, Metric};
use crate::report::{IdleReport, LoadReport, Report, ReportError, STEADY};

/// The problem of a run judged with no baseline.
const NO_BASELINE: &str = "there is no baseline; write one with --update";

/// The problem of a run judged against an entry marked for re-measurement.
const REMEASURE: &str = "the baseline entry for this host shape was measured within one run \
     (schema 2 or older); measure it again with --update before it judges a load phase";

/// The warning of an update that re-measures such an entry.
const REMEASURE_WRITING: &str =
    "the baseline entry was measured within one run; it is measured again across runs";

/// The outcome of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every check passed.
    Pass,
    /// A limit or a tolerance was breached, or a request failed.
    Fail,
    /// The run cannot be judged.
    Invalid,
}

impl Outcome {
    /// The exit code: 0, 1 or 2.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Invalid => 2,
        }
    }
}

/// One check of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// What was checked.
    pub name: String,
    /// The value measured.
    pub measured: f64,
    /// The value it was held against: the baseline, or the absolute limit.
    pub reference: f64,
    /// The worst value that still passes.
    pub threshold: f64,
    /// How the threshold was set, for a reader.
    pub rule: String,
    /// Whether it passed.
    pub passed: bool,
    /// The load phases it was judged on, a stalled phase not counted; zero
    /// for an idle limit, which one run decides.
    #[serde(default)]
    pub runs: u32,
    /// The load phases it breached in.
    #[serde(default)]
    pub breached: u32,
    /// The head over the base, in an A/B run; absent against a baseline or
    /// a limit, and where the base measured zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
    /// The product's tolerance of the metric's kind, in an A/B run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
    /// The noise floor measured within the arms, in an A/B run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<f64>,
}

impl Check {
    /// Whether one more load phase could still change this check: it
    /// breached in some run but not yet in most of three.
    #[must_use]
    pub fn undecided(&self) -> bool {
        self.runs > 0 && self.breached > 0 && self.breached < CONFIRMING && self.runs < RUNS
    }
}

/// The most load phases a breach is judged on: the first and its fresh
/// confirmations.
pub const RUNS: u32 = 3;

/// The breaches out of [`RUNS`] that fail a metric.
pub const CONFIRMING: u32 = 2;

/// The judgement of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    /// The outcome.
    pub outcome: Outcome,
    /// Every check, in order.
    pub checks: Vec<Check>,
    /// Requests the server answered other than expected, by group and kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, BTreeMap<String, u64>>,
    /// Why the run cannot be judged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// What a reader should know that changes no outcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Verdict {
    /// The checks that failed.
    pub fn breaches(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|check| !check.passed)
    }
}

/// Judges `report` against `baseline`.
///
/// With no baseline, or one of another plan, only the idle limits and the
/// failures are judged, and the run is invalid unless `writing` says a
/// baseline is being written from it.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram of the report does not
/// decode.
pub fn judge(
    report: &Report,
    baseline: Option<&Baseline>,
    writing: bool,
) -> Result<Verdict, ReportError> {
    let Findings {
        mut checks,
        mut problems,
        mut warnings,
    } = Findings::idle(&report.idle);
    let load = &report.load;
    if load.saturated {
        checks.push(Check {
            name: "load.saturation".to_owned(),
            measured: 1.0,
            reference: 0.0,
            threshold: 0.0,
            rule: "an arrival waited past the saturation limit: the server fell behind".to_owned(),
            passed: false,
            runs: 0,
            breached: 0,
            ratio: None,
            tolerance: None,
            floor: None,
        });
    }
    phase_problems(report, &mut problems)?;
    let metrics = crate::metric::of(load)?;
    match baseline {
        // NOTE: no specification governs this: our own design; a stalled first phase is named by its own problem, never as a missing baseline.
        _ if load.stalled() => {}
        None if writing => {}
        None => problems.push(NO_BASELINE.to_owned()),
        Some(base) if base.remeasure && base.plan == report.plan => {
            if writing {
                warnings.push(REMEASURE_WRITING.to_owned());
            } else {
                problems.push(REMEASURE.to_owned());
            }
        }
        Some(base) if base.plan != report.plan => {
            if writing {
                warnings.push(
                    "the baseline was written for another plan; it is written afresh".to_owned(),
                );
            } else {
                problems.push(
                    "the baseline was written for another plan; write it again with --update"
                        .to_owned(),
                );
            }
        }
        Some(base) => {
            let first = checks.len();
            baseline_checks(base, &metrics, &mut checks, &mut problems, &mut warnings);
            if let Some(judged) = checks.get_mut(first..) {
                confirm(base, report, judged)?;
            }
            let differences = base.host.differences(&report.host);
            if !differences.is_empty() {
                warnings.push(format!(
                    "the host differs from the baseline's in {}",
                    differences.join(", ")
                ));
            }
        }
    }
    let failures = all_failures(report);
    Ok(Findings {
        checks,
        problems,
        warnings,
    }
    .verdict(failures))
}

/// Judges `report` against the entry of `baselines` for the host shape of
/// the report's host.
///
/// The idle limits and the failures judge every run. With no file, or no
/// entry for the model, the load metrics are not judged, and the run is
/// invalid unless `writing` says the entry is being written from it; the
/// problem names the model and how to add its entry.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram of the report does not
/// decode.
pub fn judge_file(
    report: &Report,
    baselines: Option<&Baselines>,
    writing: bool,
) -> Result<Verdict, ReportError> {
    let shape = Shape::of(&report.host);
    let entry = baselines.and_then(|file| file.entry(shape));
    let mut verdict = judge(report, entry, writing)?;
    let Some(file) = baselines.filter(|_| entry.is_none()) else {
        return Ok(verdict);
    };
    let held = file.models();
    let held = if held.is_empty() {
        "none".to_owned()
    } else {
        held.join(", ")
    };
    let missing = format!(
        "the baseline holds no entry for the host {shape} (it holds {held}); \
         run once with --update on a runner of that shape and commit the entry"
    );
    for problem in &mut verdict.problems {
        if problem == NO_BASELINE {
            problem.clone_from(&missing);
        }
    }
    Ok(verdict)
}

fn ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "not measurable".to_owned(), |r| format!("{r:.2}"))
}

/// What a judgement found before its outcome is drawn.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    /// Every check, in order.
    pub(crate) checks: Vec<Check>,
    /// Why the run cannot be judged.
    pub(crate) problems: Vec<String>,
    /// What a reader should know that changes no outcome.
    pub(crate) warnings: Vec<String>,
}

impl Findings {
    /// The findings of the absolute idle limits of `idle`, which judge
    /// every run alike.
    pub(crate) fn idle(idle: &IdleReport) -> Self {
        let mut found = Self::default();
        idle_checks(idle, &mut found.checks, &mut found.warnings);
        found
    }

    /// The verdict: a breach or a failed request fails the run and
    /// outranks a problem, which makes it one that cannot be judged.
    pub(crate) fn verdict(self, failures: BTreeMap<String, BTreeMap<String, u64>>) -> Verdict {
        let outcome = if self.checks.iter().any(|c| !c.passed) || !failures.is_empty() {
            Outcome::Fail
        } else if !self.problems.is_empty() {
            Outcome::Invalid
        } else {
            Outcome::Pass
        };
        Verdict {
            outcome,
            checks: self.checks,
            failures,
            problems: self.problems,
            warnings: self.warnings,
        }
    }
}

fn idle_checks(idle: &IdleReport, checks: &mut Vec<Check>, warnings: &mut Vec<String>) {
    for (name, measured, limit, missing) in [
        (
            "idle.cpu_percent",
            idle.cpu_percent,
            idle.max_cpu_percent,
            "the idle CPU could not be read on this host",
        ),
        (
            "idle.transactions_per_second",
            idle.transactions_per_second,
            idle.max_transactions_per_second,
            "the idle transactions were not counted: no database was given",
        ),
    ] {
        match measured {
            Some(value) => checks.push(Check {
                name: name.to_owned(),
                measured: value,
                reference: limit,
                threshold: limit,
                rule: "absolute limit".to_owned(),
                passed: value <= limit,
                runs: 0,
                breached: 0,
                ratio: None,
                tolerance: None,
                floor: None,
            }),
            None => warnings.push(missing.to_owned()),
        }
    }
}

/// The problems of every load phase of `report`: a generator that fell
/// behind, a window that was not steady, a runner that stalled.
fn phase_problems(report: &Report, problems: &mut Vec<String>) -> Result<(), ReportError> {
    let load = &report.load;
    for phase in std::iter::once(load).chain(report.calibration.iter()) {
        schedule_problems(phase, problems)?;
    }
    for (index, phase) in std::iter::once(load)
        .chain(report.calibration.iter())
        .chain(report.confirmations.iter())
        .enumerate()
    {
        if phase.stalled() {
            problems.push(format!(
                "the runner stalled in load phase {}: every route's p99 is over {} times its p50 \
                 at once, so the phase is not judged; re-run the job on a fresh runner",
                index + 1,
                phase.stall_ratio.unwrap_or(0.0)
            ));
        }
    }
    Ok(())
}

/// The problems of one load phase's schedule: a generator that fell behind
/// and a window that was not steady.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram of the phase does not decode.
pub(crate) fn schedule_problems(
    phase: &LoadReport,
    problems: &mut Vec<String>,
) -> Result<(), ReportError> {
    if let Some(lag) = phase.lag_p99_ms
        && lag > crate::quantile::exact_f64(phase.max_lag_ms)
    {
        problems.push(format!(
            "the generator's lag p99 is {lag:.2} ms, over {} ms: it could not keep the schedule",
            phase.max_lag_ms
        ));
    }
    let steadiness = phase.steadiness()?;
    if !steadiness.steady() {
        problems.push(format!(
            "the window is not steady: the second half's p50 and p95 over the first's are \
             {} and {}, outside {} to {}",
            ratio(steadiness.p50_ratio),
            ratio(steadiness.p95_ratio),
            STEADY.0,
            STEADY.1
        ));
    }
    Ok(())
}

/// The failed requests of the first load phase and its confirmations.
fn all_failures(report: &Report) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut failures = report.load.failures.clone();
    for phase in &report.confirmations {
        add_failures(&mut failures, &phase.failures);
    }
    failures
}

/// Adds the failure counts of `more` to `into`, by group and kind.
pub(crate) fn add_failures(
    into: &mut BTreeMap<String, BTreeMap<String, u64>>,
    more: &BTreeMap<String, BTreeMap<String, u64>>,
) {
    for (group, kinds) in more {
        for (kind, count) in kinds {
            let total = into
                .entry(group.clone())
                .or_default()
                .entry(kind.clone())
                .or_insert(0);
            *total = total.saturating_add(*count);
        }
    }
}

/// Judges the load metric `checks` of the first phase again on each of
/// the report's confirmations that did not stall: a metric fails when it
/// breached in most of the runs judged, and its rule says in how many.
fn confirm(base: &Baseline, report: &Report, checks: &mut [Check]) -> Result<(), ReportError> {
    if report.confirmations.is_empty() {
        return Ok(());
    }
    for phase in report.confirmations.iter().filter(|phase| !phase.stalled()) {
        let metrics = crate::metric::of(phase)?;
        let mut again = Vec::new();
        baseline_checks(base, &metrics, &mut again, &mut Vec::new(), &mut Vec::new());
        for check in checks.iter_mut() {
            if let Some(other) = again.iter().find(|other| other.name == check.name) {
                check.runs = check.runs.saturating_add(1);
                check.breached = check.breached.saturating_add(u32::from(!other.passed));
            }
        }
    }
    for check in checks.iter_mut() {
        check.passed = check.breached.saturating_mul(2) <= check.runs;
        check.rule = format!(
            "{} (breached in {} of {} runs)",
            check.rule, check.breached, check.runs
        );
    }
    Ok(())
}

fn baseline_checks(
    base: &Baseline,
    metrics: &BTreeMap<Metric, f64>,
    checks: &mut Vec<Check>,
    problems: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for (metric, reference) in &base.metrics {
        let Some(measured) = metrics.get(metric).copied() else {
            problems.push(format!(
                "the baseline holds {metric} and this run could not measure it"
            ));
            continue;
        };
        let limit = Limit::new(
            metric.kind(),
            base.tolerance.of(metric.kind()),
            base.noise.get(metric).copied().unwrap_or(1.0),
        );
        let passed = !limit.breached(*reference, measured);
        checks.push(Check {
            name: metric.to_string(),
            measured,
            reference: *reference,
            threshold: limit.threshold(*reference),
            rule: limit.rule("the baseline"),
            passed,
            runs: 1,
            breached: u32::from(!passed),
            ratio: None,
            tolerance: None,
            floor: None,
        });
    }
    for metric in metrics.keys().filter(|m| !base.metrics.contains_key(*m)) {
        warnings.push(format!("{metric} is not in the baseline; --update adds it"));
    }
}

/// How far a metric may move from its reference: the larger of the
/// product's tolerance and the noise floor, up for a metric where higher is
/// worse and down for throughput.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Limit {
    kind: Kind,
    tolerance: f64,
    floor: f64,
    factor: f64,
}

impl Limit {
    /// The limit of a metric of `kind` with `tolerance` and noise `floor`.
    pub(crate) fn new(kind: Kind, tolerance: f64, floor: f64) -> Self {
        let factor = if kind.lower_is_worse() {
            (1.0 - tolerance).min(1.0 / floor)
        } else {
            (1.0 + tolerance).max(floor)
        };
        Self {
            kind,
            tolerance,
            floor,
            factor,
        }
    }

    /// The worst value that still passes against `reference`.
    pub(crate) fn threshold(&self, reference: f64) -> f64 {
        reference * self.factor
    }

    /// Whether `measured` is worse than `reference` by more than the limit.
    pub(crate) fn breached(&self, reference: f64, measured: f64) -> bool {
        if self.kind.lower_is_worse() {
            measured < self.threshold(reference)
        } else {
            measured > self.threshold(reference)
        }
    }

    /// The rule for a reader, against `what` the metric is held to.
    pub(crate) fn rule(&self, what: &str) -> String {
        let (bound, sign) = if self.kind.lower_is_worse() {
            ("at least", '-')
        } else {
            ("at most", '+')
        };
        format!(
            "{bound} {what} {sign}{:.0} % (noise floor {:.2})",
            self.tolerance * 100.0,
            self.floor
        )
    }
}

#[cfg(test)]
mod tests;
