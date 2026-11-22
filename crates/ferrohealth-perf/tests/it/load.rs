// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The open-loop generator against an in-process HTTP server.

use std::time::Duration;

use ferrohealth_perf::credentials::Credentials;
use ferrohealth_perf::load::{Endpoints, drive};
use ferrohealth_perf::plan::Plan;
use tokio::time::Instant;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn server(delay: Duration) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/ok"))
        .respond_with(ResponseTemplate::new(200).set_delay(delay))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/broken"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    server
}

fn plan(load: &str, requests: &str) -> Plan {
    Plan::parse(&format!("[load]\n{load}\n{requests}")).unwrap()
}

const OK: &str = "[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/ok\"\nstatus = 200\n";

#[tokio::test(flavor = "multi_thread")]
async fn every_group_is_recorded_and_a_wrong_status_is_a_failure() {
    let server = server(Duration::from_millis(5)).await;
    let broken = "[[requests]]\ngroup = \"write\"\nmethod = \"POST\"\npath = \"/broken\"\n\
                  status = 201\nbody = '{}'\nheaders = { content-type = \"application/json\" }\n";
    let plan = plan(
        "rate = 40.0\nwarmup_secs = 0\nduration_secs = 2",
        &format!("{OK}{broken}"),
    );
    let base = Url::parse(&server.uri()).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(80, outcome.planned);
    assert_eq!(80, outcome.sent);
    assert!(!outcome.saturated, "a quick server keeps up");
    let read = &outcome.groups["read"];
    assert_eq!(40, read.answered);
    assert_eq!(40, read.first.len() + read.second.len());
    assert!(
        read.first.len() >= 19 && read.second.len() >= 19,
        "both halves"
    );
    assert!(
        read.first.min() >= 5_000,
        "the latency includes the 5 ms answer"
    );
    assert_eq!(0, outcome.groups["write"].answered);
    assert_eq!(40, outcome.failures["write"]["status 500"]);
    assert!(
        !outcome.failures.contains_key("read"),
        "{:?}",
        outcome.failures
    );
    assert_eq!(80, server.received_requests().await.unwrap().len());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_warm_up_is_sent_and_not_recorded() {
    let server = server(Duration::from_millis(1)).await;
    let plan = plan("rate = 20.0\nwarmup_secs = 1\nduration_secs = 1", OK);
    let base = Url::parse(&server.uri()).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(40, outcome.sent);
    assert_eq!(20, outcome.groups["read"].answered);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_server_raises_the_latency_of_every_request_it_held_back() {
    // One slot and a 300 ms answer: the tenth arrival, intended at 0.9 s, is
    // sent near 2.7 s, so its latency from the intended instant is about
    // 2.1 s, though the server took 300 ms over it.
    let server = server(Duration::from_millis(300)).await;
    let plan = plan(
        "rate = 10.0\nwarmup_secs = 0\nduration_secs = 1\nmax_in_flight = 1\nsaturation_ms = 10000",
        OK,
    );
    let base = Url::parse(&server.uri()).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    let read = &outcome.groups["read"];
    assert_eq!(10, read.answered);
    let slowest = read.first.max().max(read.second.max());
    assert!(
        slowest >= 1_800_000,
        "the held-back arrival counts its wait: {slowest} us"
    );
    assert!(outcome.lag.max() >= 1_500_000, "the generator lagged too");
    assert!(!outcome.saturated, "no wait passed ten seconds");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_holds_every_slot_saturates_the_run() {
    let server = server(Duration::from_secs(2)).await;
    let plan = plan(
        "rate = 10.0\nwarmup_secs = 0\nduration_secs = 2\nmax_in_flight = 1\nsaturation_ms = 200",
        OK,
    );
    let base = Url::parse(&server.uri()).unwrap();
    let started = Instant::now();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        started,
    )
    .await
    .unwrap();
    assert!(outcome.saturated, "the second arrival waited past 200 ms");
    assert!(outcome.sent < outcome.planned, "the run stopped early");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_past_its_timeout_is_a_failure() {
    let server = server(Duration::from_secs(5)).await;
    let plan = plan(
        "rate = 5.0\nwarmup_secs = 0\nduration_secs = 1\ntimeout_ms = 200",
        OK,
    );
    let base = Url::parse(&server.uri()).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(5, outcome.failures["read"]["timeout"]);
    assert_eq!(0, outcome.groups["read"].answered);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_send_is_counted_from_its_intended_instant() {
    // The schedule starts 300 ms in the past, so the first arrival is sent
    // 300 ms after its intended instant: that wait is its latency and its
    // lag, though the server answers at once.
    let server = server(Duration::ZERO).await;
    let plan = plan("rate = 10.0\nwarmup_secs = 0\nduration_secs = 1", OK);
    let base = Url::parse(&server.uri()).unwrap();
    let start = Instant::now()
        .checked_sub(Duration::from_millis(300))
        .unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base.clone(), None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        start,
    )
    .await
    .unwrap();
    let read = &outcome.groups["read"];
    assert_eq!(10, read.answered);
    let lag = outcome.lag.max();
    assert!(lag >= 300_000, "the first send lagged 300 ms: {lag} us");
    let slowest = read.first.max().max(read.second.max());
    assert!(
        slowest >= lag,
        "its latency counts from the intended instant: {slowest} us"
    );
    let median = outcome.lag.value_at_quantile(0.5);
    assert!(
        median < 100_000,
        "the arrivals intended after the start carry none of its lateness: {median} us"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_phase_stops_its_schedule() {
    let server = server(Duration::ZERO).await;
    let plan = plan("rate = 50.0\nwarmup_secs = 0\nduration_secs = 10", OK);
    let base = Url::parse(&server.uri()).unwrap();
    let endpoints = Endpoints::new(&plan, base.clone(), None).unwrap();
    let client = reqwest::Client::new();
    let credentials = Credentials::default();
    let phase = drive(&plan, &endpoints, &client, &credentials, Instant::now());
    let stopped = tokio::time::timeout(Duration::from_millis(300), phase).await;
    assert!(stopped.is_err(), "the phase was still running");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_drop = server.received_requests().await.unwrap().len();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let later = server.received_requests().await.unwrap().len();
    assert!(after_drop < 50, "{after_drop} sent by then");
    assert_eq!(
        after_drop, later,
        "nothing is sent once the phase is dropped"
    );
}

/// The paths `server` received, sorted.
async fn received_paths(server: &MockServer) -> Vec<String> {
    let mut paths: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    paths.sort();
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn each_arrival_sends_its_own_draw_and_a_seed_sends_them_again() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("patients.jsonl");
    let rows: Vec<String> = (0..1_000)
        .map(|i| format!("{{\"id\":\"C-{i:07}\"}}"))
        .collect();
    let rows = rows.join("\n");
    std::fs::write(&file, rows).unwrap();
    let requests = format!(
        "[[requests]]\ngroup = \"feed\"\nmethod = \"PUT\"\npath = \"/patient/{{{{p.id}}}}\"\nstatus = 200\n\
         body = '{{\"id\":\"{{{{p.id}}}}\"}}'\n\
         [vars.p]\nkind = \"file\"\npath = {:?}\norder = \"random\"\n",
        file.display().to_string()
    );
    let plan = plan(
        "rate = 100.0\nwarmup_secs = 0\nduration_secs = 1\nseed = 3",
        &requests,
    );
    let mut runs = Vec::new();
    for _ in 0..2 {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let base = Url::parse(&server.uri()).unwrap();
        let outcome = drive(
            &plan,
            &Endpoints::new(&plan, base, None).unwrap(),
            &reqwest::Client::new(),
            &Credentials::default(),
            Instant::now(),
        )
        .await
        .unwrap();
        assert_eq!(
            100, outcome.groups["feed"].answered,
            "{:?}",
            outcome.failures
        );
        for request in server.received_requests().await.unwrap() {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                request.url.path(),
                format!("/patient/{}", body["id"].as_str().unwrap()),
                "the path and the body carry one draw"
            );
        }
        runs.push(received_paths(&server).await);
    }
    let distinct: std::collections::BTreeSet<&String> = runs[0].iter().collect();
    assert!(
        distinct.len() > 80,
        "{} distinct patients of 100",
        distinct.len()
    );
    assert_eq!(runs[0], runs[1], "one seed, one set of requests");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_templated_plan_keeps_its_schedule_at_rate() {
    // Every arrival renders a path, a header and a body from four variables,
    // 500 a second; the lag bound is not asserted here, since a busy test
    // host moves it for any plan (the draw's own cost is held in
    // template::tests).
    let server = server(Duration::ZERO).await;
    Mock::given(method("POST"))
        .and(path("/feed"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let requests = "[[requests]]\ngroup = \"feed\"\nmethod = \"POST\"\npath = \"/feed?n={{n}}\"\nstatus = 201\n\
                    headers = { x-id = \"{{u}}\" }\n\
                    body = '{\"n\":\"{{n}}\",\"r\":\"{{r}}\",\"w\":\"{{w}}\",\"u\":\"{{u}}\"}'\n\
                    [vars.n]\nkind = \"counter\"\nwidth = 9\n[vars.u]\nkind = \"uuid\"\n\
                    [vars.r]\nkind = \"random\"\nmin = 0\nmax = 999999\n\
                    [vars.w]\nkind = \"list\"\nvalues = [\"a\", \"b\", \"c\"]\norder = \"random\"\n";
    let plan = plan("rate = 500.0\nwarmup_secs = 0\nduration_secs = 2", requests);
    let base = Url::parse(&server.uri()).unwrap();
    let outcome = drive(
        &plan,
        &Endpoints::new(&plan, base, None).unwrap(),
        &reqwest::Client::new(),
        &Credentials::default(),
        Instant::now(),
    )
    .await
    .unwrap();
    assert_eq!(1_000, outcome.planned);
    assert_eq!(1_000, outcome.sent);
    assert_eq!(
        1_000, outcome.groups["feed"].answered,
        "{:?}",
        outcome.failures
    );
    let mut queries: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|request| request.url.query().map(str::to_owned))
        .collect();
    queries.sort();
    let expected: Vec<String> = (0..1_000).map(|n| format!("n={n:09}")).collect();
    assert_eq!(
        expected, queries,
        "one counter value an arrival, none twice"
    );
}
