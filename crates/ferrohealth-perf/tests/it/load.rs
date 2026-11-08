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
