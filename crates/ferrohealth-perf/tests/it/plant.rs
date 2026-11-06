// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The self-test of #249: a planted idle busy-loop fails the gate, and a
//! quiet process passes it.

use std::time::Duration;

use ferrohealth_perf::compare::Outcome;
use ferrohealth_perf::plan::Plan;
use ferrohealth_perf::process::Target;
use ferrohealth_perf::run::{RunOptions, RunResult, run};
use ferrohealth_secret::secret_url::SecretUrl;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, PLANT, PLANT_DATABASE, Planted};

/// The body of a planted process, run as this binary with only this test
/// selected and [`PLANT`] set; in an ordinary test run the variable is
/// unset and there is nothing to do.
#[test]
fn planted_process() {
    let Ok(what) = std::env::var(PLANT) else {
        return;
    };
    match what.as_str() {
        "quiet" => std::thread::sleep(Duration::from_secs(600)),
        "spin" => {
            let mut x: u64 = 0;
            loop {
                x = std::hint::black_box(x.wrapping_add(1));
            }
        }
        "commit" => {
            let url = std::env::var(PLANT_DATABASE).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls)
                    .await
                    .unwrap();
                drop(tokio::spawn(connection));
                client
                    .batch_execute("CREATE TABLE IF NOT EXISTS outbox (id bigint PRIMARY KEY)")
                    .await
                    .unwrap();
                // The FerroPIX #490 shape: an outbox sender that polls an empty
                // table in its own transaction, again and again.
                loop {
                    client
                        .execute(
                            "SELECT id FROM outbox ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED",
                            &[],
                        )
                        .await
                        .unwrap();
                }
            });
        }
        other => panic!("no plant {other}"),
    }
}

/// A server that answers `GET /ok` with 200 after 100 ms, long beside the
/// jitter of a busy test host.
async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
        .mount(&server)
        .await;
    server
}

async fn measure(target: Target, database: Option<String>, update: bool) -> RunResult {
    let server = server().await;
    let dir = tempfile::tempdir().unwrap();
    let plan = Plan::parse(&support::plan(&support::request("ok", "/ok", 200))).unwrap();
    let options = RunOptions {
        plan,
        target,
        base_url: SecretUrl::new(server.uri()),
        database_url: database.map(SecretUrl::new),
        baseline: dir.path().join("perf/baseline.toml"),
        out_dir: dir.path().join("reports"),
        update,
    };
    run(&options).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_commits_in_a_loop_while_idle_fails() {
    let Some(admin) = support::database_url() else {
        return;
    };
    let database = support::fresh_database(&admin).await;
    let busy = Planted::start("commit", Some(&database));
    let result = measure(Target::Pid(busy.pid()), Some(database), false).await;
    assert_eq!(Outcome::Fail, result.verdict.outcome, "{}", result.summary);
    let breach = result
        .verdict
        .breaches()
        .find(|check| check.name == "idle.transactions_per_second")
        .unwrap_or_else(|| panic!("the idle transactions breach: {}", result.summary));
    assert!(breach.measured > 100.0, "{breach:?}");
    assert!(
        result.summary.contains("| idle.transactions_per_second |"),
        "{}",
        result.summary
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_process_passes_and_writes_its_baseline() {
    let Some(admin) = support::database_url() else {
        return;
    };
    let database = support::fresh_database(&admin).await;
    let quiet = Planted::start("quiet", None);
    let result = measure(Target::Pid(quiet.pid()), Some(database), true).await;
    assert_eq!(Outcome::Pass, result.verdict.outcome, "{}", result.summary);
    let idle = &result.report.idle;
    assert!(idle.transactions_per_second.unwrap() < 5.0, "{idle:?}");
    let changes = result.changes.unwrap();
    assert!(!changes.is_empty(), "the baseline was written");
    assert!(result.report.calibration.is_some(), "the A/A phase ran");
    assert!(
        result.summary.contains("### Baseline"),
        "{}",
        result.summary
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_spins_while_idle_fails_where_its_cpu_can_be_read() {
    let spinning = Planted::start("spin", None);
    let result = measure(Target::Pid(spinning.pid()), None, false).await;
    if cfg!(target_os = "linux") {
        assert_eq!(Outcome::Fail, result.verdict.outcome, "{}", result.summary);
        let breach = result
            .verdict
            .breaches()
            .find(|check| check.name == "idle.cpu_percent")
            .unwrap_or_else(|| panic!("the idle CPU breach: {}", result.summary));
        assert!(breach.measured > 50.0, "one core is busy: {breach:?}");
    } else {
        assert_eq!(
            None, result.report.idle.cpu_percent,
            "no procfs, no reading"
        );
        assert!(
            result
                .verdict
                .warnings
                .iter()
                .any(|w| w.contains("idle CPU")),
            "{:?}",
            result.verdict.warnings
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_process_keeps_its_cpu_under_the_limit_where_it_can_be_read() {
    let quiet = Planted::start("quiet", None);
    let result = measure(Target::Pid(quiet.pid()), None, true).await;
    assert_eq!(Outcome::Pass, result.verdict.outcome, "{}", result.summary);
    if cfg!(target_os = "linux") {
        let cpu = result.report.idle.cpu_percent.unwrap();
        assert!(cpu < 2.0, "{cpu}");
        assert!(
            result.report.load.peak_memory_bytes.is_some(),
            "the resident set is read"
        );
    }
}
