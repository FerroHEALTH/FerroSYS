// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::baseline::{Update, updated};
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
