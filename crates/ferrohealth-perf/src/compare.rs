// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The judgement of a run: the idle limits, the failed requests, and every
//! load metric against the baseline.
//!
//! A metric breaches when it is worse than the baseline by more than the
//! larger of its tolerance and its A/A noise floor (Kalibera and Jones,
//! "Rigorous Benchmarking in Reasonable Time", ISMM 2013, on measuring the
//! noise before judging a difference); the floor follows Veredictum's
//! `perf_result::compare` (Apache-2.0, credited in `NOTICE`). A run that
//! cannot be judged (no baseline, another plan, a metric the baseline holds
//! and the run lacks, a generator that fell behind, an unsteady window) is
//! invalid. The exit code is 0 for a pass, 1 for a breach and 2 for an
//! invalid run or a harness error; a breach outranks an invalid run.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::baseline::Baseline;
use crate::metric::Metric;
use crate::report::{Report, ReportError, STEADY};

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
}

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
    let mut checks = Vec::new();
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    idle_checks(report, &mut checks, &mut warnings);
    let load = &report.load;
    if load.saturated {
        checks.push(Check {
            name: "load.saturation".to_owned(),
            measured: 1.0,
            reference: 0.0,
            threshold: 0.0,
            rule: "an arrival waited past the saturation limit: the server fell behind".to_owned(),
            passed: false,
        });
    }
    for phase in std::iter::once(load).chain(report.calibration.as_ref()) {
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
    }
    let metrics = crate::metric::of(load)?;
    match baseline {
        None if writing => {}
        None => problems.push("there is no baseline; write one with --update".to_owned()),
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
            baseline_checks(base, &metrics, &mut checks, &mut problems, &mut warnings);
            let differences = base.host.differences(&report.host);
            if !differences.is_empty() {
                warnings.push(format!(
                    "the host differs from the baseline's in {}",
                    differences.join(", ")
                ));
            }
        }
    }
    let failures = load.failures.clone();
    let outcome = if checks.iter().any(|c| !c.passed) || !failures.is_empty() {
        Outcome::Fail
    } else if !problems.is_empty() {
        Outcome::Invalid
    } else {
        Outcome::Pass
    };
    Ok(Verdict {
        outcome,
        checks,
        failures,
        problems,
        warnings,
    })
}

fn ratio(value: Option<f64>) -> String {
    value.map_or_else(|| "not measurable".to_owned(), |r| format!("{r:.2}"))
}

fn idle_checks(report: &Report, checks: &mut Vec<Check>, warnings: &mut Vec<String>) {
    let idle = &report.idle;
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
            }),
            None => warnings.push(missing.to_owned()),
        }
    }
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
        let kind = metric.kind();
        let tolerance = base.tolerance.of(kind);
        let floor = base.noise.get(metric).copied().unwrap_or(1.0);
        let (threshold, passed, rule) = if kind.lower_is_worse() {
            let threshold = (reference * (1.0 - tolerance)).min(reference / floor);
            let rule = format!(
                "at least the baseline -{:.0} % (noise floor {floor:.2})",
                tolerance * 100.0
            );
            (threshold, measured >= threshold, rule)
        } else {
            let threshold = (reference * (1.0 + tolerance)).max(reference * floor);
            let rule = format!(
                "at most the baseline +{:.0} % (noise floor {floor:.2})",
                tolerance * 100.0
            );
            (threshold, measured <= threshold, rule)
        };
        checks.push(Check {
            name: metric.to_string(),
            measured,
            reference: *reference,
            threshold,
            rule,
            passed,
        });
    }
    for metric in metrics.keys().filter(|m| !base.metrics.contains_key(*m)) {
        warnings.push(format!("{metric} is not in the baseline; --update adds it"));
    }
}

#[cfg(test)]
mod tests;
