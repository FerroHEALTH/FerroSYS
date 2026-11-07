// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::baseline::{Baselines, Update, updated};
use crate::report::fixture;

fn baseline_of(report: &Report) -> Baseline {
    let metrics = crate::metric::of(&report.load).unwrap();
    updated(
        None,
        &Update {
            metrics: &metrics,
            noise: BTreeMap::new(),
            plan: &report.plan,
            host: &report.host,
            tool: "0.0.0",
            written: "2026-11-06T10:00:00Z",
        },
    )
    .0
}

#[test]
fn a_run_equal_to_its_baseline_passes() {
    let report = fixture::report();
    let verdict = judge(&report, Some(&baseline_of(&report)), false).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
    assert_eq!(0, verdict.outcome.code());
    assert!(
        verdict.checks.len() >= 9,
        "two idle checks and the load metrics"
    );
    assert!(verdict.warnings.is_empty(), "{:?}", verdict.warnings);
}

#[test]
fn a_latency_past_its_tolerance_fails_and_names_both_values() {
    let report = fixture::report();
    let baseline = baseline_of(&report);
    let mut slower = report.clone();
    slower.load = fixture::load(13, 250);
    let verdict = judge(&slower, Some(&baseline), false).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    assert_eq!(1, verdict.outcome.code());
    let breach = verdict
        .breaches()
        .find(|c| c.name == "read.p50_ms")
        .unwrap();
    assert!((breach.reference - 10.0).abs() < 0.05, "{breach:?}");
    assert!((breach.measured - 13.0).abs() < 0.05, "{breach:?}");
    assert!(
        (breach.threshold - breach.reference * 1.25).abs() < 1e-9,
        "{breach:?}"
    );
}

#[test]
fn a_latency_within_its_tolerance_passes() {
    let report = fixture::report();
    let baseline = baseline_of(&report);
    let mut slower = report.clone();
    slower.load = fixture::load(12, 250);
    assert_eq!(
        Outcome::Pass,
        judge(&slower, Some(&baseline), false).unwrap().outcome
    );
}

#[test]
fn the_noise_floor_widens_a_tolerance_it_exceeds() {
    let report = fixture::report();
    let mut baseline = baseline_of(&report);
    let p50 = Metric::try_from("read.p50_ms".to_owned()).unwrap();
    baseline.noise.insert(p50, 1.4);
    let mut slower = report.clone();
    slower.load = fixture::load(13, 250);
    let verdict = judge(&slower, Some(&baseline), false).unwrap();
    assert!(
        verdict.breaches().all(|c| c.name != "read.p50_ms"),
        "1.3 is inside the floor of 1.4: {verdict:?}"
    );
}

#[test]
fn throughput_fails_when_it_falls_past_its_tolerance() {
    let report = fixture::report();
    let baseline = baseline_of(&report);
    let mut fewer = report.clone();
    fewer.load.measured_secs = 14.0;
    let verdict = judge(&fewer, Some(&baseline), false).unwrap();
    let breach = verdict
        .breaches()
        .find(|c| c.name == "throughput_rps")
        .unwrap();
    assert!((breach.threshold - 40.0).abs() < 1e-9, "{breach:?}");
    fewer.load.measured_secs = 12.0;
    let verdict = judge(&fewer, Some(&baseline), false).unwrap();
    assert!(
        verdict.breaches().all(|c| c.name != "throughput_rps"),
        "{verdict:?}"
    );
}

#[test]
fn an_idle_limit_is_absolute() {
    let mut report = fixture::report();
    report.idle.transactions_per_second = Some(8_000.0);
    let verdict = judge(&report, Some(&baseline_of(&fixture::report())), false).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    let breach = verdict.breaches().next().unwrap();
    assert_eq!("idle.transactions_per_second", breach.name);
    assert!((breach.threshold - 5.0).abs() < f64::EPSILON, "{breach:?}");
    report.idle.transactions_per_second = Some(0.1);
    report.idle.cpu_percent = Some(2.5);
    let verdict = judge(&report, None, true).unwrap();
    assert_eq!("idle.cpu_percent", verdict.breaches().next().unwrap().name);
}

#[test]
fn an_unmeasured_idle_reading_is_a_warning() {
    let mut report = fixture::report();
    report.idle.cpu_percent = None;
    report.idle.transactions_per_second = None;
    let verdict = judge(&report, None, true).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome);
    assert_eq!(2, verdict.warnings.len(), "{:?}", verdict.warnings);
}

#[test]
fn a_failed_request_fails_the_run() {
    let mut report = fixture::report();
    report.load.failures.insert(
        "read".to_owned(),
        BTreeMap::from([("status 500".to_owned(), 3)]),
    );
    let verdict = judge(&report, Some(&baseline_of(&fixture::report())), false).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    assert_eq!(3, verdict.failures["read"]["status 500"]);
}

#[test]
fn saturation_fails_the_run() {
    let mut report = fixture::report();
    report.load.saturated = true;
    let verdict = judge(&report, None, true).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
}

#[test]
fn no_baseline_is_invalid_unless_one_is_being_written() {
    let report = fixture::report();
    let verdict = judge(&report, None, false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert_eq!(2, verdict.outcome.code());
    assert_eq!(Outcome::Pass, judge(&report, None, true).unwrap().outcome);
}

#[test]
fn another_plan_is_invalid() {
    let report = fixture::report();
    let mut baseline = baseline_of(&report);
    baseline.plan.rate = 10.0;
    let verdict = judge(&report, Some(&baseline), false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert!(
        verdict.problems[0].contains("another plan"),
        "{:?}",
        verdict.problems
    );
    let writing = judge(&report, Some(&baseline), true).unwrap();
    assert_eq!(Outcome::Pass, writing.outcome, "an update writes it afresh");
    assert!(
        writing.warnings[0].contains("afresh"),
        "{:?}",
        writing.warnings
    );
}

#[test]
fn a_metric_the_run_lacks_is_invalid() {
    let report = fixture::report();
    let baseline = baseline_of(&report);
    let mut blind = report.clone();
    blind.load.cpu_ms = None;
    let verdict = judge(&blind, Some(&baseline), false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert!(
        verdict.problems[0].contains("cpu_per_request_ms"),
        "{:?}",
        verdict.problems
    );
}

#[test]
fn a_lagging_generator_or_an_unsteady_window_is_invalid() {
    let report = fixture::report();
    let baseline = baseline_of(&report);
    let mut lagging = report.clone();
    lagging.load.lag_p99_ms = Some(50.0);
    let verdict = judge(&lagging, Some(&baseline), false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert!(
        verdict.problems[0].contains("lag"),
        "{:?}",
        verdict.problems
    );
    let mut unsteady = report.clone();
    let slow = fixture::load(20, 250);
    let first = unsteady.load.groups["read"].first_half.clone();
    let group = unsteady.load.groups.get_mut("read").unwrap();
    group.first_half = first;
    group.second_half = slow.groups["read"].second_half.clone();
    let verdict = judge(&unsteady, None, true).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert!(
        verdict.problems[0].contains("not steady"),
        "{:?}",
        verdict.problems
    );
}

#[test]
fn another_host_is_a_warning() {
    let report = fixture::report();
    let mut baseline = baseline_of(&report);
    baseline.host.available_parallelism = Some(64);
    let verdict = judge(&report, Some(&baseline), false).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome);
    assert!(
        verdict.warnings[0].contains("available_parallelism"),
        "{:?}",
        verdict.warnings
    );
}

fn on_model(mut report: Report, model: &str) -> Report {
    report.host.cpu_model = Some(model.to_owned());
    report
}

fn file_of(entries: Vec<Baseline>) -> Baselines {
    entries.into_iter().fold(
        Baselines {
            schema: crate::baseline::SCHEMA,
            entries: Vec::new(),
        },
        Baselines::with,
    )
}

#[test]
fn a_run_is_judged_against_the_entry_of_its_cpu_model() {
    let fast = on_model(fixture::report(), "EPYC 9V45");
    let mut slow = on_model(fixture::report(), "EPYC 7763");
    slow.load = fixture::load(20, 250);
    let file = file_of(vec![baseline_of(&fast), baseline_of(&slow)]);
    // The slow model is twice the fast one's latency, and passes on its own entry.
    let verdict = judge_file(&slow, Some(&file), false).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
    // Against the fast entry the same run would breach.
    let against_fast = judge(&slow, file.entry(Some("EPYC 9V45")), false).unwrap();
    assert_eq!(Outcome::Fail, against_fast.outcome);
}

#[test]
fn a_model_with_no_entry_cannot_be_judged_and_is_named() {
    let known = on_model(fixture::report(), "EPYC 9V45");
    let file = file_of(vec![baseline_of(&known)]);
    let unknown = on_model(fixture::report(), "EPYC 7763");
    let verdict = judge_file(&unknown, Some(&file), false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert_eq!(2, verdict.outcome.code());
    let problem = &verdict.problems[0];
    assert!(
        problem.contains("no entry for the CPU model `EPYC 7763`"),
        "{problem}"
    );
    assert!(problem.contains("it holds `EPYC 9V45`"), "{problem}");
    assert!(problem.contains("--update"), "{problem}");
    assert!(
        verdict.checks.iter().all(|c| c.name.starts_with("idle.")),
        "no load metric is judged: {:?}",
        verdict.checks
    );
    assert_eq!(
        Outcome::Pass,
        judge_file(&unknown, Some(&file), true).unwrap().outcome,
        "an update writes the model's entry"
    );
}

#[test]
fn the_idle_limits_judge_a_model_with_no_entry() {
    let known = on_model(fixture::report(), "EPYC 9V45");
    let file = file_of(vec![baseline_of(&known)]);
    let mut busy = on_model(fixture::report(), "EPYC 7763");
    busy.idle.transactions_per_second = Some(8_000.0);
    let verdict = judge_file(&busy, Some(&file), false).unwrap();
    assert_eq!(
        Outcome::Fail,
        verdict.outcome,
        "a breach outranks the missing entry"
    );
    assert_eq!(
        "idle.transactions_per_second",
        verdict.breaches().next().unwrap().name
    );
    let none = judge_file(&busy, None, false).unwrap();
    assert_eq!(Outcome::Fail, none.outcome, "and so does a missing file");
}

/// The entry an update over phases whose p99 tails took `tails` writes.
fn entry_across(tails: &[u64]) -> Baseline {
    let report = fixture::report();
    let phases: Vec<_> = tails
        .iter()
        .map(|tail| crate::metric::of(&fixture::tailed(10, *tail, 1000)).unwrap())
        .collect();
    let (metrics, noise) = crate::baseline::across(&phases);
    updated(
        None,
        &Update {
            metrics: &metrics,
            noise,
            plan: &report.plan,
            host: &report.host,
            tool: "0.0.0",
            written: "2026-11-07T10:00:00Z",
        },
    )
    .0
}

fn run_with_tail(tail_ms: u64) -> Report {
    let mut report = fixture::report();
    report.load = fixture::tailed(10, tail_ms, 1000);
    report
}

#[test]
fn a_noisy_p99_inside_the_spread_across_runs_passes() {
    // Five phases of one commit: the p99 tail moved from 20 to 26 ms.
    let entry = entry_across(&[20, 20, 26, 21, 20]);
    let p99 = Metric::try_from("read.p99_ms".to_owned()).unwrap();
    let floor = *entry.noise.get(&p99).unwrap();
    assert!(floor > 1.25, "the spread is past the tolerance: {floor}");
    // A run whose tail reached 26 ms, 30 % over the median, is that same noise.
    let verdict = judge(&run_with_tail(26), Some(&entry), false).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
}

#[test]
fn a_real_2x_regression_of_the_tail_still_fails() {
    let entry = entry_across(&[20, 20, 26, 21, 20]);
    let verdict = judge(&run_with_tail(40), Some(&entry), false).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    let breached: Vec<_> = verdict.breaches().map(|c| c.name.as_str()).collect();
    assert_eq!(vec!["read.p99_ms"], breached, "the p50 and p95 hold");
}

#[test]
fn an_entry_marked_for_re_measurement_judges_no_load_phase() {
    let report = fixture::report();
    let mut entry = baseline_of(&report);
    entry.remeasure = true;
    let verdict = judge(&report, Some(&entry), false).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome);
    assert!(
        verdict.problems[0].contains("measure it again with --update"),
        "{:?}",
        verdict.problems
    );
    assert!(verdict.checks.iter().all(|c| c.name.starts_with("idle.")));
    let writing = judge(&report, Some(&entry), true).unwrap();
    assert_eq!(Outcome::Pass, writing.outcome);
    let mut busy = report;
    busy.idle.cpu_percent = Some(50.0);
    assert_eq!(
        Outcome::Fail,
        judge(&busy, Some(&entry), false).unwrap().outcome,
        "the idle limits still judge"
    );
}
