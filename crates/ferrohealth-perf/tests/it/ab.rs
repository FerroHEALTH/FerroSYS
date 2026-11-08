// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The binary's A/B mode: two servers side by side, the head judged over
//! the base, the report re-judged offline.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use ferrohealth_perf::ab::{AbReport, Arm};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Planted};

async fn server(delay_ms: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(delay_ms)))
        .mount(&server)
        .await;
    server
}

fn perf(args: &[&str], step_summary: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ferrohealth-perf"))
        .args(args)
        .env("GITHUB_STEP_SUMMARY", step_summary)
        .env_remove("FERROHEALTH_PERF_BASE_DATABASE_URL")
        .env_remove("FERROHEALTH_PERF_HEAD_DATABASE_URL")
        .output()
        .unwrap()
}

/// Runs `ab` with the base answering after `base_ms` and the head after
/// `head_ms`, over two pairs; returns the output and the report.
async fn ab(base_ms: u64, head_ms: u64) -> (Output, AbReport, tempfile::TempDir) {
    let (base, head) = (server(base_ms).await, server(head_ms).await);
    let (base_process, head_process) =
        (Planted::start("quiet", None), Planted::start("quiet", None));
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.toml");
    std::fs::write(&plan, support::plan(&support::request("ok", "/ok", 200))).unwrap();
    let reports = dir.path().join("reports");
    let (base_pid, head_pid) = (
        base_process.pid().to_string(),
        head_process.pid().to_string(),
    );
    let (base_uri, head_uri) = (base.uri(), head.uri());
    let output = perf(
        &[
            "ab",
            "--plan",
            plan.to_str().unwrap(),
            "--base-url",
            &base_uri,
            "--base-pid",
            &base_pid,
            "--head-url",
            &head_uri,
            "--head-pid",
            &head_pid,
            "--pairs",
            "2",
            "--out-dir",
            reports.to_str().unwrap(),
        ],
        &dir.path().join("step.md"),
    );
    let report = AbReport::read(&reports.join("latest.json")).unwrap();
    (output, report, dir)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_head_as_fast_as_its_base_passes() {
    let (output, report, _dir) = ab(40, 40).await;
    assert_eq!(
        Some(0),
        output.status.code(),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(text(&output.stdout).contains("## ferrohealth-perf: the A/B run passed"));
    assert_eq!(2, report.pairs.len());
    assert_eq!(
        vec![Arm::Base, Arm::Head],
        report.pairs.iter().map(|p| p.first).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slower_head_fails_and_compare_judges_the_report_again() {
    let (output, report, dir) = ab(20, 80).await;
    let stdout = text(&output.stdout);
    assert_eq!(
        Some(1),
        output.status.code(),
        "{stdout}{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("### The head over the base"), "{stdout}");
    assert!(stdout.contains("| ok.p50_ms |"), "{stdout}");
    let verdict = report.verdict.unwrap();
    assert!(
        verdict.breaches().any(|c| c.name == "ok.p50_ms"),
        "{verdict:?}"
    );

    let latest = dir.path().join("reports/latest.json");
    let again = perf(
        &["compare", "--report", latest.to_str().unwrap()],
        &dir.path().join("step.md"),
    );
    assert_eq!(Some(1), again.status.code(), "{}", text(&again.stderr));
    assert!(text(&again.stdout).contains("the A/B run failed"));
}

#[test]
fn ab_needs_both_servers_and_an_even_count_of_pairs() {
    let dir = tempfile::tempdir().unwrap();
    let step = dir.path().join("s.md");
    let both = [
        "ab",
        "--base-url",
        "http://127.0.0.1:9",
        "--head-url",
        "http://127.0.0.1:9",
    ];
    let missing = perf(&[&both[..], &["--base-pid", "1"]].concat(), &step);
    assert_eq!(Some(2), missing.status.code());
    assert!(
        text(&missing.stderr).contains("--head-pid"),
        "{}",
        text(&missing.stderr)
    );
    let one = perf(
        &[
            &both[..],
            &["--base-pid", "1", "--head-pid", "1", "--pairs", "1"],
        ]
        .concat(),
        &step,
    );
    assert_eq!(Some(2), one.status.code());
    assert!(
        text(&one.stderr).contains("--pairs"),
        "{}",
        text(&one.stderr)
    );
    for odd in ["3", "5"] {
        let refused = perf(
            &[
                &both[..],
                &["--base-pid", "1", "--head-pid", "1", "--pairs", odd],
            ]
            .concat(),
            &step,
        );
        assert_eq!(Some(2), refused.status.code());
        let stderr = text(&refused.stderr);
        assert!(
            stderr.contains("--pairs") && stderr.contains("not an even count of at least 2"),
            "{stderr}"
        );
    }
}

/// A server that answers every `GET` with 200, so the paths it received
/// show which values its arm sent.
async fn any_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// The paths `server` received, in order.
async fn paths(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect()
}

/// Asserts that `output` is a run the judge passed or could not judge on
/// this runner's timing, never a breach, and returns its exit code; the
/// verdict on timing is the judging tests' subject (#291).
fn passed_or_unjudged(output: &Output) -> i32 {
    let code = output.status.code();
    let stdout = text(&output.stdout);
    match code {
        Some(0) => assert!(stdout.contains("the A/B run passed"), "{stdout}"),
        Some(2) => assert!(
            stdout.contains("the A/B run could not be judged"),
            "a setup error, not a refusal on timing: {stdout}{}",
            text(&output.stderr)
        ),
        other => panic!("exit {other:?}: {stdout}{}", text(&output.stderr)),
    }
    code.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_arm_reads_the_id_its_own_values_name() {
    let (base, head) = (any_server().await, any_server().await);
    let (base_process, head_process) =
        (Planted::start("quiet", None), Planted::start("quiet", None));
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.toml");
    std::fs::write(
        &plan,
        support::plan(&support::request("item", "/item/@ITEM_ID@", 200)),
    )
    .unwrap();
    let reports = dir.path().join("reports");
    let (base_pid, head_pid) = (
        base_process.pid().to_string(),
        head_process.pid().to_string(),
    );
    let (base_uri, head_uri) = (base.uri(), head.uri());
    let output = perf(
        &[
            "ab",
            "--plan",
            plan.to_str().unwrap(),
            "--base-url",
            &base_uri,
            "--base-pid",
            &base_pid,
            "--base-var",
            "ITEM_ID=b-1",
            "--head-url",
            &head_uri,
            "--head-pid",
            &head_pid,
            "--head-var",
            "ITEM_ID=h-1",
            "--pairs",
            "2",
            "--out-dir",
            reports.to_str().unwrap(),
        ],
        &dir.path().join("step.md"),
    );
    let code = passed_or_unjudged(&output);
    let stdout = text(&output.stdout);
    for (server, wanted) in [(&base, "/item/b-1"), (&head, "/item/h-1")] {
        let sent = paths(server).await;
        assert!(
            !sent.is_empty() && sent.iter().all(|path| path == wanted),
            "{wanted}: {sent:?}"
        );
    }
    assert!(
        stdout.contains("The base's plan values: `ITEM_ID` = `b-1`.")
            && stdout.contains("The head's plan values: `ITEM_ID` = `h-1`."),
        "{stdout}"
    );
    let latest = reports.join("latest.json");
    let report = AbReport::read(&latest).unwrap();
    assert!(
        report
            .pairs
            .iter()
            .all(|pair| pair.head.failures.is_empty() && pair.base.failures.is_empty())
    );
    let values = ferrohealth_perf::ab::recorded_values(&latest).unwrap();
    assert_eq!(Some("b-1"), values.base.get("ITEM_ID"));
    assert_eq!(Some("h-1"), values.head.get("ITEM_ID"));

    let again = perf(
        &["compare", "--report", latest.to_str().unwrap()],
        &dir.path().join("step.md"),
    );
    assert_eq!(Some(code), again.status.code(), "{}", text(&again.stderr));
    assert!(text(&again.stdout).contains("The head's plan values: `ITEM_ID` = `h-1`."));
}

#[test]
fn a_placeholder_one_arm_leaves_unfilled_refuses_the_run_before_any_load() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.toml");
    std::fs::write(
        &plan,
        support::plan(&support::request("item", "/item/@ITEM_ID@", 200)),
    )
    .unwrap();
    let reports = dir.path().join("reports");
    let base = [
        "ab",
        "--plan",
        plan.to_str().unwrap(),
        "--base-url",
        "http://127.0.0.1:9",
        "--base-pid",
        "1",
        "--head-url",
        "http://127.0.0.1:9",
        "--head-pid",
        "1",
        "--out-dir",
        reports.to_str().unwrap(),
    ];
    let step = dir.path().join("s.md");
    for (extra, words) in [
        (
            vec!["--base-var", "ITEM_ID=b-1"],
            vec![
                "the head's plan values are refused",
                "@ITEM_ID@ has no value",
            ],
        ),
        (
            vec!["--head-var", "ITEM_ID=h-1"],
            vec![
                "the base's plan values are refused",
                "@ITEM_ID@ has no value",
            ],
        ),
        (
            vec![
                "--base-var",
                "ITEM_ID=b-1",
                "--base-var",
                "OTHER=x",
                "--head-var",
                "ITEM_ID=h-1",
            ],
            vec![
                "the base's plan values are refused",
                "OTHER fills no placeholder",
            ],
        ),
        (
            vec!["--base-var", "item_id=b-1"],
            vec!["--base-var", "is not NAME=VALUE"],
        ),
    ] {
        let refused = perf(&[&base[..], extra.as_slice()].concat(), &step);
        let stderr = text(&refused.stderr);
        assert_eq!(Some(2), refused.status.code(), "{extra:?}: {stderr}");
        for word in words {
            assert!(stderr.contains(word), "{extra:?}: {stderr}");
        }
        assert!(!reports.exists(), "{extra:?}: no phase ran");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_two_phases_of_a_pair_take_turns_at_once() {
    let server = any_server().await;
    let (base_process, head_process) =
        (Planted::start("quiet", None), Planted::start("quiet", None));
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.toml");
    std::fs::write(
        &plan,
        support::plan(&support::request("item", "/item/@ARM@", 200)),
    )
    .unwrap();
    let reports = dir.path().join("reports");
    let (base_pid, head_pid) = (
        base_process.pid().to_string(),
        head_process.pid().to_string(),
    );
    let uri = server.uri();
    let output = perf(
        &[
            "ab",
            "--plan",
            plan.to_str().unwrap(),
            "--base-url",
            &uri,
            "--base-pid",
            &base_pid,
            "--base-var",
            "ARM=base",
            "--head-url",
            &uri,
            "--head-pid",
            &head_pid,
            "--head-var",
            "ARM=head",
            "--pairs",
            "2",
            "--out-dir",
            reports.to_str().unwrap(),
        ],
        &dir.path().join("step.md"),
    );
    passed_or_unjudged(&output);
    let arms = paths(&server).await;
    let turns = arms.windows(2).filter(|two| two[0] != two[1]).count();
    assert!(
        arms.len() > 400 && turns * 10 > arms.len() * 9,
        "{turns} turns in {} requests: the arms sent one after the other",
        arms.len()
    );
}
