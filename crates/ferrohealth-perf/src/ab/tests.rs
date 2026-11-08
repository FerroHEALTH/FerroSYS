// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::compare::Outcome;
use crate::report::fixture;

/// An A/B report whose pair `i` measured the base at `base[i]` ms and the
/// head at `head[i]` ms, in alternating order.
fn ab(base: &[u64], head: &[u64]) -> AbReport {
    let run = fixture::report();
    let pairs = base
        .iter()
        .zip(head)
        .zip(0..)
        .map(|((b, h), index)| Pair {
            first: Arm::first_of(index),
            base: fixture::load(*b, 250),
            head: fixture::load(*h, 250),
        })
        .collect();
    AbReport {
        schema: SCHEMA,
        tool: "0.0.0".to_owned(),
        started: run.started,
        finished: run.finished,
        host: run.host,
        plan: run.plan,
        tolerance: Tolerances::default(),
        base: TargetInfo {
            process: "process 2".to_owned(),
            base_url: "http://127.0.0.1:8081".to_owned(),
            database: Some("base".to_owned()),
            mllp: None,
        },
        head: run.target,
        idle: run.idle,
        pairs,
        verdict: None,
    }
}

fn check<'a>(verdict: &'a Verdict, name: &str) -> &'a Check {
    verdict.checks.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn equal_arms_pass() {
    let verdict = judge(&ab(&[10, 10, 10], &[10, 10, 10])).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
    let p50 = check(&verdict, "read.p50_ms");
    assert!((p50.ratio.unwrap() - 1.0).abs() < 1e-9, "{p50:?}");
    assert_eq!(Some(0.25), p50.tolerance);
    assert_eq!(Some(1.0), p50.floor);
    assert_eq!((3, 0), (p50.runs, p50.breached));
    assert!(verdict.warnings.is_empty(), "{:?}", verdict.warnings);
}

#[test]
fn a_real_slowdown_fails_in_every_pair() {
    let verdict = judge(&ab(&[10, 10, 10], &[14, 14, 14])).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome, "{verdict:?}");
    assert_eq!(1, verdict.outcome.code());
    let p50 = check(&verdict, "read.p50_ms");
    assert!(!p50.passed, "{p50:?}");
    assert_eq!((3, 3), (p50.runs, p50.breached));
    assert!((p50.reference - 10.0).abs() < 0.05, "{p50:?}");
    assert!((p50.measured - 14.0).abs() < 0.05, "{p50:?}");
    assert!(
        (p50.threshold - p50.reference * 1.25).abs() < 1e-9,
        "{p50:?}"
    );
    assert!(
        p50.rule
            .starts_with("at most the base +25 % (noise floor 1.00); breached in 3 of 3"),
        "{}",
        p50.rule
    );
}

#[test]
fn a_slowdown_within_the_tolerance_passes() {
    let verdict = judge(&ab(&[10, 10, 10], &[12, 12, 12])).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
}

#[test]
fn noise_within_the_floor_passes() {
    // The base strays to 14 in one phase, so its floor is 1.4 and a head
    // 30 % slower stays within it.
    let verdict = judge(&ab(&[10, 10, 14], &[13, 13, 13])).unwrap();
    let p50 = check(&verdict, "read.p50_ms");
    assert!(p50.floor.unwrap() > 1.39, "{p50:?}");
    assert!(p50.passed, "{p50:?}");
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
}

#[test]
fn a_breach_in_one_pair_of_three_passes() {
    // One head phase strays: the floor widens, and one pair of three is no
    // majority.
    let verdict = judge(&ab(&[10, 10, 10], &[10, 20, 10])).unwrap();
    let p50 = check(&verdict, "read.p50_ms");
    assert!(p50.passed, "{p50:?}");
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
}

#[test]
fn the_alternation_order_is_recorded() {
    assert_eq!(
        vec![Arm::Base, Arm::Head, Arm::Base, Arm::Head],
        (0..4).map(Arm::first_of).collect::<Vec<_>>()
    );
    let report = ab(&[10, 10, 10], &[10, 10, 10]);
    let firsts: Vec<Arm> = report.pairs.iter().map(|pair| pair.first).collect();
    assert_eq!(vec![Arm::Base, Arm::Head, Arm::Base], firsts);
    let text = serde_json::to_string(&report).unwrap();
    assert!(text.contains("\"first\":\"head\""), "{text}");
    let summary = summary::ab_markdown(&report, &judge(&report).unwrap());
    assert!(
        summary.contains("(the base leads; the head leads; the base leads)"),
        "{summary}"
    );
}

#[test]
fn a_stalled_phase_cannot_be_judged_and_is_left_out() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    report.pairs[1].head = fixture::tailed(10, 500, 1000);
    let verdict = judge(&report).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome, "{verdict:?}");
    assert_eq!(2, verdict.outcome.code());
    assert!(
        verdict
            .problems
            .iter()
            .any(|p| p.starts_with("the head's phase of pair 2: the runner stalled")),
        "{:?}",
        verdict.problems
    );
    let p99 = check(&verdict, "read.p99_ms");
    assert_eq!(2, p99.runs, "the stalled pair is not judged: {p99:?}");
}

#[test]
fn an_unsteady_window_cannot_be_judged() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    let mut slow_half = fixture::load(10, 250);
    let other = fixture::load(30, 250);
    let group = slow_half.groups.get_mut("read").unwrap();
    group
        .second_half
        .clone_from(&other.groups["read"].second_half);
    report.pairs[0].base = slow_half;
    let verdict = judge(&report).unwrap();
    assert_eq!(Outcome::Invalid, verdict.outcome, "{verdict:?}");
    assert!(
        verdict
            .problems
            .iter()
            .any(|p| p.starts_with("the base's phase of pair 1: the window is not steady")),
        "{:?}",
        verdict.problems
    );
}

#[test]
fn the_idle_check_is_absolute_against_the_head() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    report.idle.transactions_per_second = Some(8_000.0);
    let verdict = judge(&report).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    let idle = check(&verdict, "idle.transactions_per_second");
    assert_eq!("absolute limit", idle.rule);
    assert!(idle.tolerance.is_none(), "{idle:?}");
}

#[test]
fn a_head_failure_fails_and_a_base_failure_is_a_note() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    report.pairs[0].base.failures.insert(
        "read".to_owned(),
        BTreeMap::from([("status 404".to_owned(), 3)]),
    );
    let verdict = judge(&report).unwrap();
    assert_eq!(Outcome::Pass, verdict.outcome, "{verdict:?}");
    assert!(
        verdict
            .warnings
            .iter()
            .any(|w| w.contains("the base answered 3 read request(s)")),
        "{:?}",
        verdict.warnings
    );
    report.pairs[2].head.failures.insert(
        "read".to_owned(),
        BTreeMap::from([("status 500".to_owned(), 1)]),
    );
    let verdict = judge(&report).unwrap();
    assert_eq!(Outcome::Fail, verdict.outcome);
    assert_eq!(Some(&1), verdict.failures["read"].get("status 500"));
}

#[test]
fn a_saturated_head_fails_and_a_saturated_base_cannot_be_judged() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    report.pairs[0].head.saturated = true;
    let verdict = judge(&report).unwrap();
    assert!(!check(&verdict, "load.saturation").passed);
    assert_eq!(Outcome::Fail, verdict.outcome);
    report.pairs[1].base.saturated = true;
    let verdict = judge(&report).unwrap();
    assert!(verdict.checks.iter().all(|c| c.name != "load.saturation"));
    assert_eq!(Outcome::Invalid, verdict.outcome, "{verdict:?}");
}

#[test]
fn a_metric_one_arm_lacks_is_a_note() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    for pair in &mut report.pairs {
        let group = pair.head.groups.remove("read").unwrap();
        pair.head.groups.insert("new".to_owned(), group);
    }
    let verdict = judge(&report).unwrap();
    assert!(
        verdict
            .warnings
            .iter()
            .any(|w| w == "new.p50_ms was not measured on the base, so it is not judged"),
        "{:?}",
        verdict.warnings
    );
    assert!(
        verdict
            .warnings
            .iter()
            .any(|w| w == "read.p50_ms was measured on the base only, so it is not judged"),
        "{:?}",
        verdict.warnings
    );
}

#[test]
fn the_summary_shows_base_head_ratio_tolerance_and_floor() {
    let report = ab(&[10, 10, 10], &[14, 14, 14]);
    let verdict = judge(&report).unwrap();
    let text = summary::ab_markdown(&report, &verdict);
    assert!(
        text.starts_with("## ferrohealth-perf: the A/B run failed\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "| Metric | Base | Head | Ratio | Tolerance | Floor | Threshold | Breached | Result |"
        ),
        "{text}"
    );
    assert!(
        text.contains("| read.p50_ms | 10.007 | 14.007 | 1.40 | 25 % | 1.00 |"),
        "{text}"
    );
    assert!(text.contains("| 3 of 3 pairs | FAIL |"), "{text}");
    assert!(text.contains("| idle.cpu_percent |"), "{text}");
}

#[test]
fn a_report_round_trips_through_its_file() {
    let mut report = ab(&[10, 10, 10], &[10, 10, 10]);
    report.verdict = Some(judge(&report).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let path = crate::report::write(&report, dir.path(), "20261108T100000Z").unwrap();
    assert_eq!(report, AbReport::read(&path).unwrap());
    assert_eq!(
        report,
        AbReport::read(&dir.path().join("latest.json")).unwrap()
    );
    let mut newer = serde_json::to_value(&report).unwrap();
    newer["schema"] = serde_json::Value::from(9);
    std::fs::write(&path, newer.to_string()).unwrap();
    assert!(matches!(AbReport::read(&path), Err(ReportError::Schema(9))));
}

#[test]
fn a_run_measures_an_even_count_of_pairs() {
    assert_eq!(
        vec![2, 2, 2, 4, 4, 6, 6],
        [0, 1, 2, 3, 4, 5, 6].map(even_pairs).to_vec()
    );
    assert_eq!(u32::MAX - 1, even_pairs(u32::MAX));
    assert!(DEFAULT_PAIRS.is_multiple_of(2));
}
