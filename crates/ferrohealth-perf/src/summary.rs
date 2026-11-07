// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The Markdown summary of a run, for `$GITHUB_STEP_SUMMARY` and a reader.
//!
//! It names the outcome first, then every breach with the metric, the
//! baseline, the measured value and the threshold, then each request
//! group's quantiles with their 95 % intervals. GitHub renders the file as
//! GitHub Flavored Markdown ("Adding a job summary", GitHub Actions
//! documentation).

use crate::ab::AbReport;
use crate::compare::{Check, Outcome, Verdict};
use crate::quantile::{Quantile, QuantileName};
use crate::report::Report;

/// The summary of `report` judged as `verdict`.
#[must_use]
pub fn markdown(report: &Report, verdict: &Verdict) -> String {
    let mut out = String::new();
    line(
        &mut out,
        &format!("## ferrohealth-perf: the run {}", headline(verdict)),
    );
    line(&mut out, "");
    line(
        &mut out,
        &format!(
            "{} against `{}`, measured on {} {} from {} to {}.",
            report.target.process,
            report.target.base_url,
            report.host.os,
            report.host.arch,
            report.started,
            report.finished
        ),
    );
    line(&mut out, "");
    if !report.confirmations.is_empty() {
        line(
            &mut out,
            &format!(
                "A load breach was judged again on {} fresh load phase(s); a metric fails \
                 only when it breached in most of the runs.",
                report.confirmations.len()
            ),
        );
        line(&mut out, "");
    }
    list(&mut out, "Why it could not be judged", &verdict.problems);
    let breaches: Vec<&Check> = verdict.breaches().collect();
    if !breaches.is_empty() {
        line(&mut out, "### Breaches");
        line(&mut out, "");
        table(&mut out, &breaches);
    }
    failures(&mut out, verdict);
    line(&mut out, "### Checks");
    line(&mut out, "");
    let all: Vec<&Check> = verdict.checks.iter().collect();
    table(&mut out, &all);
    line(&mut out, "### Latency by group");
    line(&mut out, "");
    line(&mut out, "| Group | Answered | p50 ms | p95 ms | p99 ms |");
    line(&mut out, "| --- | ---: | --- | --- | --- |");
    for (group, record) in &report.load.groups {
        let cell = |name| {
            record
                .quantiles
                .get(&name)
                .map_or_else(|| "absent".to_owned(), quantile_cell)
        };
        line(
            &mut out,
            &format!(
                "| {group} | {} | {} | {} | {} |",
                record.answered,
                cell(QuantileName::P50),
                cell(QuantileName::P95),
                cell(QuantileName::P99)
            ),
        );
    }
    line(&mut out, "");
    let idle = &report.idle;
    if let (Some(start), Some(end)) = (idle.memory_start_bytes, idle.memory_end_bytes) {
        line(
            &mut out,
            &format!("Memory while idle went from {start} to {end} bytes."),
        );
        line(&mut out, "");
    }
    list(&mut out, "Notes", &verdict.warnings);
    out
}

/// The summary of the A/B run `report` judged as `verdict`: the head's idle
/// limits, then each load metric with the base, the head, their ratio, the
/// tolerance and the noise floor.
#[must_use]
pub fn ab_markdown(report: &AbReport, verdict: &Verdict) -> String {
    let mut out = String::new();
    line(
        &mut out,
        &format!("## ferrohealth-perf: the A/B run {}", headline(verdict)),
    );
    line(&mut out, "");
    let order: Vec<String> = report
        .pairs
        .iter()
        .map(|pair| format!("{} then {}", pair.first, pair.first.other()))
        .collect();
    line(
        &mut out,
        &format!(
            "The head, {} against `{}`, judged against the base, {} against `{}`, over {} \
             pairs of load phases ({}), measured on {} {} from {} to {}.",
            report.head.process,
            report.head.base_url,
            report.base.process,
            report.base.base_url,
            report.pairs.len(),
            order.join("; "),
            report.host.os,
            report.host.arch,
            report.started,
            report.finished
        ),
    );
    line(&mut out, "");
    list(&mut out, "Why it could not be judged", &verdict.problems);
    let (compared, limits): (Vec<&Check>, Vec<&Check>) = verdict
        .checks
        .iter()
        .partition(|check| check.tolerance.is_some());
    let breaches: Vec<&Check> = compared.iter().copied().filter(|c| !c.passed).collect();
    if !breaches.is_empty() {
        line(&mut out, "### Breaches");
        line(&mut out, "");
        ab_table(&mut out, &breaches);
    }
    failures(&mut out, verdict);
    line(&mut out, "### The head at rest and under load");
    line(&mut out, "");
    table(&mut out, &limits);
    line(&mut out, "### The head over the base");
    line(&mut out, "");
    ab_table(&mut out, &compared);
    list(&mut out, "Notes", &verdict.warnings);
    out
}

fn headline(verdict: &Verdict) -> &'static str {
    match verdict.outcome {
        Outcome::Pass => "passed",
        Outcome::Fail => "failed",
        Outcome::Invalid => "could not be judged",
    }
}

fn failures(out: &mut String, verdict: &Verdict) {
    if verdict.failures.is_empty() {
        return;
    }
    line(out, "### Requests answered other than expected");
    line(out, "");
    line(out, "| Group | Failure | Count |");
    line(out, "| --- | --- | ---: |");
    for (group, kinds) in &verdict.failures {
        for (kind, count) in kinds {
            line(out, &format!("| {group} | {kind} | {count} |"));
        }
    }
    line(out, "");
}

fn ab_table(out: &mut String, checks: &[&Check]) {
    line(
        out,
        "| Metric | Base | Head | Ratio | Tolerance | Floor | Threshold | Breached | Result |",
    );
    line(
        out,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
    );
    for check in checks {
        let optional =
            |value: Option<f64>| value.map_or_else(|| "n/a".to_owned(), |v| format!("{v:.2}"));
        let row = format!(
            "| {} | {} | {} | {} | {:.0} % | {} | {} | {} of {} pairs | {} |",
            check.name,
            number(check.reference),
            number(check.measured),
            optional(check.ratio),
            check.tolerance.unwrap_or(0.0) * 100.0,
            optional(check.floor),
            number(check.threshold),
            check.breached,
            check.runs,
            if check.passed { "pass" } else { "FAIL" }
        );
        line(out, &row);
    }
    line(out, "");
}

fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

fn list(out: &mut String, heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    line(out, &format!("### {heading}"));
    line(out, "");
    for item in items {
        line(out, &format!("- {item}"));
    }
    line(out, "");
}

fn table(out: &mut String, checks: &[&Check]) {
    line(
        out,
        "| Check | Measured | Baseline or limit | Threshold | Rule | Result |",
    );
    line(out, "| --- | ---: | ---: | ---: | --- | --- |");
    for check in checks {
        let row = format!(
            "| {} | {} | {} | {} | {} | {} |",
            check.name,
            number(check.measured),
            number(check.reference),
            number(check.threshold),
            check.rule,
            if check.passed { "pass" } else { "FAIL" }
        );
        line(out, &row);
    }
    line(out, "");
}

fn number(value: f64) -> String {
    if value.abs() >= 1_000.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    }
}

fn quantile_cell(quantile: &Quantile) -> String {
    match quantile {
        Quantile::Resolved {
            value_ms,
            low_ms,
            high_ms,
            ..
        } => format!("{value_ms:.3} ({low_ms:.3} to {high_ms:.3})"),
        Quantile::Withheld { samples, needed } => {
            format!("withheld ({samples} of {needed} samples)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::judge;
    use crate::report::fixture;

    #[test]
    fn a_failed_run_names_its_breach_first() {
        let mut report = fixture::report();
        report.idle.transactions_per_second = Some(8_000.0);
        let verdict = judge(&report, None, true).unwrap();
        let text = markdown(&report, &verdict);
        assert!(
            text.starts_with("## ferrohealth-perf: the run failed\n"),
            "{text}"
        );
        let breaches = text.find("### Breaches").unwrap();
        let checks = text.find("### Checks").unwrap();
        assert!(breaches < checks, "{text}");
        assert!(
            text.contains(
                "| idle.transactions_per_second | 8000 | 5.000 | 5.000 | absolute limit | FAIL |"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_passed_run_shows_each_group_with_its_intervals() {
        let report = fixture::report();
        let verdict = judge(&report, None, true).unwrap();
        let text = markdown(&report, &verdict);
        assert!(
            text.starts_with("## ferrohealth-perf: the run passed\n"),
            "{text}"
        );
        assert!(!text.contains("### Breaches"), "{text}");
        assert!(
            text.contains("| read | 500 | 10.007 (10.007 to 10.007) |"),
            "{text}"
        );
        assert!(
            text.contains("Memory while idle went from 90000000 to 90000000 bytes."),
            "{text}"
        );
    }

    #[test]
    fn an_invalid_run_says_why() {
        let report = fixture::report();
        let verdict = judge(&report, None, false).unwrap();
        let text = markdown(&report, &verdict);
        assert!(text.contains("could not be judged"), "{text}");
        assert!(
            text.contains("- there is no baseline; write one with --update"),
            "{text}"
        );
    }

    #[test]
    fn a_withheld_quantile_names_its_samples() {
        assert_eq!(
            "withheld (5 of 100 samples)",
            quantile_cell(&Quantile::Withheld {
                samples: 5,
                needed: 100
            })
        );
    }
}
