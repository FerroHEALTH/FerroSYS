// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The binary: `run`, `run --update`, the offline `compare`, the report
//! files, the summary and the exit codes.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use ferrohealth_perf::baseline::{Baseline, Baselines};
use ferrohealth_perf::metric::Metric;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Planted};

fn perf(args: &[&str], step_summary: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ferrohealth-perf"))
        .args(args)
        .env("GITHUB_STEP_SUMMARY", step_summary)
        .env_remove(support::DATABASE)
        .env_remove("FERROHEALTH_PERF_DATABASE_URL")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario whose steps read and write one baseline file in order"
)]
async fn update_then_compare_then_a_regression_then_a_refused_update() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
        .mount(&server)
        .await;
    let quiet = Planted::start("quiet", None);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let plan = root.join("plan.toml");
    std::fs::write(&plan, support::plan(&support::request("ok", "/ok", 200))).unwrap();
    let baseline = root.join("perf/baseline.toml");
    let reports = root.join("reports");
    let summary = root.join("summary.md");
    let step = root.join("step-summary.md");
    let pid = quiet.pid().to_string();
    let uri = server.uri();
    let run = |update: bool| {
        let mut args = vec![
            "run",
            "--plan",
            plan.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
            "--pid",
            &pid,
            "--base-url",
            &uri,
            "--out-dir",
            reports.to_str().unwrap(),
            "--summary",
            summary.to_str().unwrap(),
        ];
        if update {
            args.extend(["--update", "--runs", "3"]);
        }
        perf(&args, &step)
    };

    // With no baseline a plain run cannot be judged.
    let first = run(false);
    assert_eq!(Some(2), first.status.code(), "{}", text(&first.stdout));
    assert!(text(&first.stdout).contains("there is no baseline"));

    // --update writes it.
    let written = run(true);
    assert_eq!(
        Some(0),
        written.status.code(),
        "{}{}",
        text(&written.stdout),
        text(&written.stderr)
    );
    let file = Baselines::read(&baseline).unwrap().unwrap();
    assert_eq!(1, file.entries.len(), "one entry, the runner's model");
    let stored = file.entries.first().unwrap().clone();
    assert!(!stored.noise.is_empty(), "the A/A floor is stored");
    let only = |entry: Baseline| Baselines {
        schema: file.schema,
        entries: vec![entry],
    };
    let latest = reports.join("latest.json");
    assert!(latest.is_file());
    let measured = ferrohealth_perf::report::Report::read(&latest).unwrap();
    assert_eq!(
        2,
        measured.calibration.len(),
        "--runs 3 measured three load phases"
    );
    let dated = std::fs::read_dir(&reports)
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().starts_with("report-")
        })
        .count();
    assert!(dated >= 1, "a dated report beside latest.json");
    assert!(
        std::fs::read_to_string(&summary)
            .unwrap()
            .contains("the run passed")
    );
    assert!(
        std::fs::read_to_string(&step)
            .unwrap()
            .contains("the run passed")
    );

    // The saved report compares offline against it.
    let report = latest.to_str().unwrap();
    let base = baseline.to_str().unwrap();
    let compared = perf(&["compare", "--report", report, "--baseline", base], &step);
    assert_eq!(
        Some(0),
        compared.status.code(),
        "{}",
        text(&compared.stdout)
    );

    // A baseline far faster than the run makes the report a regression.
    let mut strict = stored.clone();
    let p50 = Metric::try_from("ok.p50_ms".to_owned()).unwrap();
    strict.metrics.insert(p50, 1.0);
    strict.noise.clear();
    only(strict.clone()).write(&baseline).unwrap();
    let regressed = perf(&["compare", "--report", report, "--baseline", base], &step);
    assert_eq!(
        Some(1),
        regressed.status.code(),
        "{}",
        text(&regressed.stdout)
    );
    assert!(
        text(&regressed.stdout).contains("| ok.p50_ms |"),
        "{}",
        text(&regressed.stdout)
    );

    // --update refuses a run that fails, and leaves the file alone.
    let refused = run(true);
    assert_eq!(Some(1), refused.status.code(), "{}", text(&refused.stdout));
    assert!(text(&refused.stdout).contains("--update was refused"));
    assert_eq!(
        Some(only(strict.clone())),
        Baselines::read(&baseline).unwrap()
    );

    // A file with no entry for the runner's model cannot judge the report.
    let mut elsewhere = strict.clone();
    elsewhere.host.cpu_model = Some("A CPU no runner has".to_owned());
    only(elsewhere).write(&baseline).unwrap();
    let unknown = perf(&["compare", "--report", report, "--baseline", base], &step);
    assert_eq!(Some(2), unknown.status.code(), "{}", text(&unknown.stdout));
    assert!(
        text(&unknown.stdout).contains("no entry for the CPU model"),
        "{}",
        text(&unknown.stdout)
    );

    // A baseline of another plan cannot judge the report.
    let mut other = strict;
    other.plan.rate = 1.0;
    only(other).write(&baseline).unwrap();
    let mismatched = perf(&["compare", "--report", report, "--baseline", base], &step);
    assert_eq!(
        Some(2),
        mismatched.status.code(),
        "{}",
        text(&mismatched.stdout)
    );
    assert!(text(&mismatched.stdout).contains("another plan"));
}

#[test]
fn a_harness_error_exits_2_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let step = dir.path().join("step.md");
    let missing = dir.path().join("absent.toml");
    let output = perf(
        &[
            "run",
            "--plan",
            missing.to_str().unwrap(),
            "--pid",
            "1",
            "--base-url",
            "http://127.0.0.1:9",
        ],
        &step,
    );
    assert_eq!(Some(2), output.status.code());
    assert!(
        text(&output.stderr).contains("cannot read the plan"),
        "{}",
        text(&output.stderr)
    );

    let report = dir.path().join("report.json");
    std::fs::write(&report, "{\"schema\": 1}").unwrap();
    let output = perf(&["compare", "--report", report.to_str().unwrap()], &step);
    assert_eq!(Some(2), output.status.code());
    assert!(
        text(&output.stderr).contains("does not parse"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn runs_takes_at_least_two_phases_and_only_with_update() {
    let dir = tempfile::tempdir().unwrap();
    let step = dir.path().join("s.md");
    let base = ["run", "--pid", "1", "--base-url", "http://127.0.0.1:9"];
    for extra in [&["--update", "--runs", "1"][..], &["--runs", "3"][..]] {
        let args: Vec<&str> = base.iter().chain(extra).copied().collect();
        let output = perf(&args, &step);
        assert_eq!(Some(2), output.status.code(), "{extra:?}");
        assert!(
            text(&output.stderr).contains("--runs"),
            "{extra:?}: {}",
            text(&output.stderr)
        );
    }
}

#[test]
fn a_run_needs_a_server_process() {
    let dir = tempfile::tempdir().unwrap();
    let output = perf(
        &["run", "--base-url", "http://127.0.0.1:9"],
        &dir.path().join("s.md"),
    );
    assert_eq!(Some(2), output.status.code(), "clap refuses the arguments");
    assert!(
        text(&output.stderr).contains("--pid"),
        "{}",
        text(&output.stderr)
    );
}
