// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

use super::*;
use crate::quantile::QuantileName;

fn plan() -> PlanIdentity {
    PlanIdentity {
        rate: 50.0,
        warmup_secs: 10,
        duration_secs: 60,
        requests: 2,
        groups: vec!["read".to_owned()],
    }
}

fn host() -> Fingerprint {
    Fingerprint {
        arch: "x86_64".to_owned(),
        os: "linux".to_owned(),
        available_parallelism: Some(4),
        cpu_model: None,
        total_memory_bytes: None,
    }
}

fn p95() -> Metric {
    Metric::Latency {
        group: "read".to_owned(),
        quantile: QuantileName::P95,
    }
}

fn metrics(p95_ms: f64, rps: f64) -> BTreeMap<Metric, f64> {
    BTreeMap::from([(p95(), p95_ms), (Metric::Throughput, rps)])
}

fn update<'a>(
    metrics: &'a BTreeMap<Metric, f64>,
    plan: &'a PlanIdentity,
    host: &'a Fingerprint,
) -> Update<'a> {
    Update {
        metrics,
        noise: BTreeMap::from([(p95(), 1.05)]),
        plan,
        host,
        tool: "0.1.0",
        written: "2026-11-06T10:00:00Z",
    }
}

#[test]
fn a_first_update_writes_the_run_with_the_default_tolerances() {
    let (plan, host, run) = (plan(), host(), metrics(4.0, 50.0));
    let (baseline, changes) = updated(None, &update(&run, &plan, &host));
    assert_eq!(run, baseline.metrics);
    assert_eq!(Tolerances::default(), baseline.tolerance);
    assert_eq!(2, changes.len());
    assert!(
        changes.iter().all(|c| matches!(c, Change::Added(..))),
        "{changes:?}"
    );
}

#[test]
fn an_update_on_the_same_plan_only_tightens() {
    let (plan, host) = (plan(), host());
    let first = metrics(4.0, 50.0);
    let (mut old, _) = updated(None, &update(&first, &plan, &host));
    old.tolerance.latency = 0.4;
    // Latency got better, throughput got worse: only the latency moves.
    let run = metrics(3.0, 45.0);
    let (new, changes) = updated(Some(&old), &update(&run, &plan, &host));
    assert_eq!(Some(&3.0), new.metrics.get(&p95()));
    assert_eq!(Some(&50.0), new.metrics.get(&Metric::Throughput));
    assert!(
        (new.tolerance.latency - 0.4).abs() < f64::EPSILON,
        "the tolerance stays"
    );
    assert!(changes.contains(&Change::Tightened {
        metric: p95(),
        from: 4.0,
        to: 3.0
    }));
    assert!(changes.contains(&Change::Kept(Metric::Throughput, 50.0)));
}

#[test]
fn an_update_on_another_plan_rewrites() {
    let (plan, host) = (plan(), host());
    let first = metrics(4.0, 50.0);
    let (old, _) = updated(None, &update(&first, &plan, &host));
    let mut other = plan.clone();
    other.rate = 100.0;
    let run = metrics(9.0, 99.0);
    let (new, changes) = updated(Some(&old), &update(&run, &other, &host));
    assert_eq!(run, new.metrics);
    assert_eq!(vec![Change::Rewritten], changes);
}

#[test]
fn a_baseline_round_trips_through_its_file() {
    let (plan, host, run) = (plan(), host(), metrics(4.0, 50.0));
    let (baseline, _) = updated(None, &update(&run, &plan, &host));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perf/baseline.toml");
    baseline.write(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"read.p95_ms\" = 4.0"), "{text}");
    assert!(text.starts_with("# The performance baseline"), "{text}");
    assert_eq!(Some(baseline), Baseline::read(&path).unwrap());
    assert_eq!(
        None,
        Baseline::read(&dir.path().join("absent.toml")).unwrap()
    );
}

#[test]
fn the_module_example_parses() {
    let text = "schema = 1\ntool = \"0.1.0\"\nwritten = \"2026-11-06T10:00:00Z\"\n\
                [plan]\nrate = 50.0\nwarmup_secs = 10\nduration_secs = 60\nrequests = 2\n\
                groups = [\"create\", \"metadata\"]\n[host]\narch = \"x86_64\"\nos = \"linux\"\n\
                [tolerance]\nlatency = 0.25\n[metrics]\nthroughput_rps = 49.9\n\
                \"metadata.p95_ms\" = 4.1\n[noise]\n\"metadata.p95_ms\" = 1.08\n";
    let baseline = Baseline::parse(text).unwrap();
    assert_eq!(2, baseline.metrics.len());
    assert!(
        (baseline.tolerance.memory - 0.20).abs() < f64::EPSILON,
        "a default"
    );
}

#[test]
fn a_newer_or_missing_schema_is_refused() {
    assert!(matches!(
        Baseline::parse("schema = 2\n"),
        Err(BaselineError::Schema(2))
    ));
    assert!(matches!(
        Baseline::parse("tool = \"x\"\n"),
        Err(BaselineError::NoSchema)
    ));
    assert!(matches!(
        Baseline::parse("schema = \"1\"\n"),
        Err(BaselineError::NoSchema)
    ));
}

#[test]
fn a_bad_value_is_refused() {
    let (plan, host, run) = (plan(), host(), metrics(4.0, 50.0));
    let (baseline, _) = updated(None, &update(&run, &plan, &host));
    let text = baseline.to_toml().unwrap();
    for (from, to) in [
        ("\"read.p95_ms\" = 1.05", "\"read.p95_ms\" = 0.5"),
        ("throughput_rps = 50.0", "throughput_rps = -1.0"),
        ("throughput = 0.2", "throughput = 1.0"),
        ("schema = 1", "schema = 1\nextra = 1"),
    ] {
        assert!(text.contains(from), "{from} in {text}");
        let bad = text.replacen(from, to, 1);
        assert!(Baseline::parse(&bad).is_err(), "{to} is refused");
    }
}

#[test]
fn the_noise_floor_is_the_larger_ratio() {
    let a = metrics(4.0, 50.0);
    let b = metrics(5.0, 40.0);
    let floor = noise_floor(&a, &b);
    assert!(
        (floor.get(&p95()).unwrap() - 1.25).abs() < 1e-9,
        "{floor:?}"
    );
    assert!(
        (floor.get(&Metric::Throughput).unwrap() - 1.25).abs() < 1e-9,
        "{floor:?}"
    );
    let zero = BTreeMap::from([(Metric::TransactionsPerRequest, 0.0)]);
    assert!(noise_floor(&zero, &zero).is_empty(), "zero gives no ratio");
}
