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

fn host_of(model: &str) -> Fingerprint {
    Fingerprint {
        cpu_model: Some(model.to_owned()),
        ..host()
    }
}

fn file_of(entries: Vec<Baseline>) -> Baselines {
    entries.into_iter().fold(
        Baselines {
            schema: SCHEMA,
            entries: Vec::new(),
        },
        Baselines::with,
    )
}

#[test]
fn a_baseline_file_round_trips_through_its_file() {
    let (plan, run) = (plan(), metrics(4.0, 50.0));
    let (fast, _) = updated(None, &update(&run, &plan, &host_of("EPYC 9V45")));
    let (slow, _) = updated(None, &update(&run, &plan, &host_of("EPYC 7763")));
    let file = file_of(vec![fast, slow]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("perf/baseline.toml");
    file.write(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"read.p95_ms\" = 4.0"), "{text}");
    assert!(text.starts_with("# The performance baseline"), "{text}");
    assert_eq!(
        2,
        text.lines().filter(|l| *l == "[[baseline]]").count(),
        "{text}"
    );
    assert!(
        text.find("EPYC 7763").unwrap() < text.find("EPYC 9V45").unwrap(),
        "the entries are in the order of their model: {text}"
    );
    assert_eq!(Some(file), Baselines::read(&path).unwrap());
    assert_eq!(
        None,
        Baselines::read(&dir.path().join("absent.toml")).unwrap()
    );
}

#[test]
fn the_entry_of_a_model_is_found_and_another_model_has_none() {
    let (plan, run) = (plan(), metrics(4.0, 50.0));
    let (entry, _) = updated(None, &update(&run, &plan, &host_of("EPYC 9V45")));
    let file = file_of(vec![entry.clone()]);
    assert_eq!(Some(&entry), file.entry(Some("EPYC 9V45")));
    assert_eq!(None, file.entry(Some("EPYC 7763")));
    assert_eq!(None, file.entry(None), "a host with no model name");
    assert_eq!(vec!["`EPYC 9V45`".to_owned()], file.models());
}

#[test]
fn an_entry_replaces_only_the_entry_of_its_model() {
    let plan = plan();
    let (fast, _) = updated(
        None,
        &update(&metrics(4.0, 50.0), &plan, &host_of("EPYC 9V45")),
    );
    let (slow, _) = updated(
        None,
        &update(&metrics(6.0, 50.0), &plan, &host_of("EPYC 7763")),
    );
    let file = file_of(vec![fast, slow.clone()]);
    let run = metrics(3.0, 50.0);
    let (tighter, _) = updated(
        file.entry(Some("EPYC 9V45")),
        &update(&run, &plan, &host_of("EPYC 9V45")),
    );
    let file = file.with(tighter);
    assert_eq!(2, file.entries.len());
    assert_eq!(
        Some(&3.0),
        file.entry(Some("EPYC 9V45")).unwrap().metrics.get(&p95())
    );
    assert_eq!(Some(&slow), file.entry(Some("EPYC 7763")), "untouched");
}

#[test]
fn a_schema_1_file_migrates_into_one_entry_of_its_model() {
    let text = "schema = 1\ntool = \"0.1.0\"\nwritten = \"2026-11-06T10:00:00Z\"\n\
                [plan]\nrate = 50.0\nwarmup_secs = 10\nduration_secs = 60\nrequests = 2\n\
                groups = [\"create\", \"metadata\"]\n[host]\narch = \"x86_64\"\nos = \"linux\"\n\
                cpu_model = \"AMD EPYC 9V45 96-Core Processor\"\n\
                [tolerance]\nlatency = 0.25\n[metrics]\nthroughput_rps = 49.9\n\
                \"metadata.p95_ms\" = 4.1\n[noise]\n\"metadata.p95_ms\" = 1.08\n";
    let file = Baselines::parse(text).unwrap();
    assert_eq!(SCHEMA, file.schema);
    assert_eq!(1, file.entries.len());
    let entry = file.entry(Some("AMD EPYC 9V45 96-Core Processor")).unwrap();
    assert_eq!(2, entry.metrics.len());
    assert_eq!("0.1.0", entry.tool);
    assert!(
        (entry.tolerance.memory - 0.20).abs() < f64::EPSILON,
        "a default"
    );
    let again = Baselines::parse(&file.to_toml().unwrap()).unwrap();
    assert_eq!(file, again, "the migrated file is written as schema 2");
}

#[test]
fn the_module_example_parses() {
    let text = "schema = 2\n[[baseline]]\ntool = \"0.2.0\"\nwritten = \"2026-11-07T10:00:00Z\"\n\
                [baseline.plan]\nrate = 50.0\nwarmup_secs = 10\nduration_secs = 60\nrequests = 2\n\
                groups = [\"create\", \"metadata\"]\n[baseline.host]\narch = \"x86_64\"\n\
                os = \"linux\"\ncpu_model = \"AMD EPYC 7763 64-Core Processor\"\n\
                [baseline.tolerance]\nlatency = 0.25\n[baseline.metrics]\nthroughput_rps = 49.9\n\
                \"metadata.p95_ms\" = 4.1\n[baseline.noise]\n\"metadata.p95_ms\" = 1.08\n";
    let file = Baselines::parse(text).unwrap();
    assert_eq!(2, file.entries.first().unwrap().metrics.len());
}

#[test]
fn a_newer_or_missing_schema_is_refused() {
    assert!(matches!(
        Baselines::parse("schema = 3\n"),
        Err(BaselineError::Schema(3))
    ));
    assert!(matches!(
        Baselines::parse("tool = \"x\"\n"),
        Err(BaselineError::NoSchema)
    ));
    assert!(matches!(
        Baselines::parse("schema = \"1\"\n"),
        Err(BaselineError::NoSchema)
    ));
}

#[test]
fn a_bad_value_or_a_model_named_twice_is_refused() {
    let (plan, run) = (plan(), metrics(4.0, 50.0));
    let (entry, _) = updated(None, &update(&run, &plan, &host_of("EPYC 7763")));
    let text = file_of(vec![entry]).to_toml().unwrap();
    for (from, to) in [
        ("\"read.p95_ms\" = 1.05", "\"read.p95_ms\" = 0.5"),
        ("throughput_rps = 50.0", "throughput_rps = -1.0"),
        ("throughput = 0.2", "throughput = 1.0"),
        ("schema = 2", "schema = 2\nextra = 1"),
    ] {
        assert!(text.contains(from), "{from} in {text}");
        let bad = text.replacen(from, to, 1);
        assert!(Baselines::parse(&bad).is_err(), "{to} is refused");
    }
    let body = text.split_once("\n[[baseline]]").unwrap().1;
    let twice = format!("{text}\n[[baseline]]{body}");
    let refused = Baselines::parse(&twice).unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("two entries for the CPU model `EPYC 7763`"),
        "{refused}"
    );
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
