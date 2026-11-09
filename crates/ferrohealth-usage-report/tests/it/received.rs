// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A received report read back under the rules of envelope v2, with the
//! vendored schema and its examples as the oracle.

use std::path::{Path, PathBuf};

use ferrohealth_usage_report::envelope::{Event, Product, RequestsBucket};
use ferrohealth_usage_report::received::{Kind, Pattern, Rule, read};
use serde_json::{Value, json};

fn schemas() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/specs/ferropulse/schemas")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("the vendored file reads"))
        .expect("JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&read_json(&schemas().join("report-envelope.v2.json")))
        .expect("the schema compiles")
}

fn examples(validity: &str) -> Vec<(String, Value)> {
    let dir = schemas().join("examples/report-envelope.v2").join(validity);
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .expect("the examples are vendored")
        .map(|entry| {
            let path = entry.expect("an entry").path();
            let name = path
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, read_json(&path))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn body(report: &Value) -> Vec<u8> {
    serde_json::to_vec(report).expect("serializes")
}

fn daily() -> Value {
    json!({
        "schema": 2,
        "event": "daily",
        "product": "ferropix",
        "instance_id": "0f8fad5b-d9cb-469f-a165-70867728950e",
        "version": "0.4.1-rc.1+build.7",
        "git_sha": "1a2b3c4",
        "licence": "commercial",
        "deployment": "helm",
        "uptime_s": 86_400,
        "cpu_bucket": "5-8",
        "memory_bucket": "16-32",
        "status": {"auth": true, "domains": 3, "store": "postgres", "lanes": ["pdqm", "pixm"]},
        "metrics": {"window_24h": {
            "requests_bucket": "1-100",
            "groups": {"pdqm_search": {
                "p50_ms": 12.5, "p95_ms": 40.0, "p99_ms": 80.25, "errors_5xx": 1,
                "histogram": [0, 0, 3, 1, 0, 0, 0, 0, 0, 0, 0]
            }},
            "counters": {"responses_4xx": 1}
        }}
    })
}

/// Sets the member at `pointer` of `report` to `value`, or removes it when
/// `value` is `None`.
fn with(mut report: Value, pointer: &str, value: Option<Value>) -> Value {
    let (parent, name) = pointer.rsplit_once('/').expect("a pointer");
    let target = report.pointer_mut(parent).expect("the parent exists");
    match (target, value) {
        (Value::Object(map), Some(value)) => {
            map.insert(name.to_owned(), value);
        }
        (Value::Object(map), None) => {
            map.remove(name);
        }
        (Value::Array(items), Some(value)) => {
            let index: usize = name.parse().expect("an index");
            *items.get_mut(index).expect("the item exists") = value;
        }
        _ => panic!("no member {pointer}"),
    }
    report
}

#[test]
fn every_valid_example_reads_and_every_invalid_one_is_refused() {
    let valid = examples("valid");
    assert!(!valid.is_empty(), "the valid examples are vendored");
    for (name, example) in valid {
        let report = read(&body(&example));
        assert!(report.is_ok(), "{name} reads: {:?}", report.err());
    }
    let invalid = examples("invalid");
    assert!(!invalid.is_empty(), "the invalid examples are vendored");
    for (name, example) in invalid {
        assert!(read(&body(&example)).is_err(), "{name} is refused");
    }
}

#[test]
fn a_daily_report_reads_into_the_closed_types() {
    let report = read(&body(&daily())).expect("the report reads");
    assert_eq!(Product::Pix, report.product);
    assert_eq!("0.4.1-rc.1+build.7", report.version);
    let Event::Daily { metrics } = report.event else {
        panic!("a daily report");
    };
    let window = metrics.window_24h;
    assert_eq!(RequestsBucket::Below100, window.requests_bucket);
    let group = window.groups.values().next().expect("one group");
    assert_eq!([0, 0, 3, 1, 0, 0, 0, 0, 0, 0, 0], group.histogram);
    assert_eq!(
        Some(&json!(["pdqm", "pixm"])),
        report.status.as_map().get("lanes")
    );
}

/// A refused report: its name, the report, the member refused and the
/// rule it breaks.
type Case = (&'static str, Value, &'static str, fn(&Rule) -> bool);

#[expect(clippy::too_many_lines, reason = "one table of the refusal cases")]
fn refusals() -> Vec<Case> {
    vec![
        (
            "unknown top",
            with(daily(), "/extra", Some(json!(1))),
            "/extra",
            |r| matches!(r, Rule::Unknown),
        ),
        (
            "missing",
            with(daily(), "/licence", None),
            "/licence",
            |r| matches!(r, Rule::Missing),
        ),
        (
            "schema",
            with(daily(), "/schema", Some(json!(3))),
            "/schema",
            |r| matches!(r, Rule::Const),
        ),
        (
            "schema text",
            with(daily(), "/schema", Some(json!("2"))),
            "/schema",
            |r| matches!(r, Rule::Type(Kind::Integer)),
        ),
        (
            "first_start on daily",
            with(daily(), "/first_start", Some(json!(true))),
            "/first_start",
            |r| matches!(r, Rule::OtherEvent),
        ),
        (
            "daily without metrics",
            with(daily(), "/metrics", None),
            "/metrics",
            |r| matches!(r, Rule::Missing),
        ),
        (
            "product",
            with(daily(), "/product", Some(json!("ferroaudit"))),
            "/product",
            |r| matches!(r, Rule::NotInSet(_)),
        ),
        (
            "instance id",
            with(daily(), "/instance_id", Some(json!("not-a-uuid"))),
            "/instance_id",
            |r| matches!(r, Rule::Pattern(Pattern::InstanceId)),
        ),
        (
            "version",
            with(daily(), "/version", Some(json!("1.2"))),
            "/version",
            |r| matches!(r, Rule::Pattern(Pattern::Version)),
        ),
        (
            "git sha",
            with(daily(), "/git_sha", Some(json!("ABCDEF1"))),
            "/git_sha",
            |r| matches!(r, Rule::Pattern(Pattern::GitSha)),
        ),
        (
            "uptime",
            with(daily(), "/uptime_s", Some(json!(315_576_001))),
            "/uptime_s",
            |r| matches!(r, Rule::Maximum),
        ),
        (
            "negative uptime",
            with(daily(), "/uptime_s", Some(json!(-1))),
            "/uptime_s",
            |r| matches!(r, Rule::Minimum),
        ),
        (
            "status key",
            with(daily(), "/status/Auth", Some(json!(true))),
            "/status/Auth",
            |r| matches!(r, Rule::Pattern(Pattern::Key)),
        ),
        (
            "status null",
            with(daily(), "/status/auth", Some(Value::Null)),
            "/status/auth",
            |r| matches!(r, Rule::StatusValue),
        ),
        (
            "status free text",
            with(daily(), "/status/store", Some(json!("Post gres"))),
            "/status/store",
            |r| matches!(r, Rule::Pattern(Pattern::Token)),
        ),
        (
            "status fraction",
            with(daily(), "/status/domains", Some(json!(2.5))),
            "/status/domains",
            |r| matches!(r, Rule::Type(Kind::Integer)),
        ),
        (
            "status repeat",
            with(daily(), "/status/lanes", Some(json!(["a", "a"]))),
            "/status/lanes",
            |r| matches!(r, Rule::NotUnique),
        ),
        (
            "status array item",
            with(daily(), "/status/lanes", Some(json!(["a", 1]))),
            "/status/lanes/1",
            |r| matches!(r, Rule::Type(Kind::String)),
        ),
        (
            "window member",
            with(daily(), "/metrics/window_24h/extra", Some(json!({}))),
            "/metrics/window_24h/extra",
            |r| matches!(r, Rule::Unknown),
        ),
        (
            "requests bucket",
            with(
                daily(),
                "/metrics/window_24h/requests_bucket",
                Some(json!("1-99")),
            ),
            "/metrics/window_24h/requests_bucket",
            |r| matches!(r, Rule::NotInSet(_)),
        ),
        (
            "group latency",
            with(
                daily(),
                "/metrics/window_24h/groups/pdqm_search/p95_ms",
                Some(json!(600_001)),
            ),
            "/metrics/window_24h/groups/pdqm_search/p95_ms",
            |r| matches!(r, Rule::Maximum),
        ),
        (
            "group missing",
            with(
                daily(),
                "/metrics/window_24h/groups/pdqm_search/p99_ms",
                None,
            ),
            "/metrics/window_24h/groups/pdqm_search/p99_ms",
            |r| matches!(r, Rule::Missing),
        ),
        (
            "histogram short",
            with(
                daily(),
                "/metrics/window_24h/groups/pdqm_search/histogram",
                Some(json!([0, 1])),
            ),
            "/metrics/window_24h/groups/pdqm_search/histogram",
            |r| matches!(r, Rule::Buckets),
        ),
        (
            "histogram bucket",
            with(
                daily(),
                "/metrics/window_24h/groups/pdqm_search/histogram/4",
                Some(json!(10_000_001)),
            ),
            "/metrics/window_24h/groups/pdqm_search/histogram/4",
            |r| matches!(r, Rule::Maximum),
        ),
        (
            "counter key",
            with(daily(), "/metrics/window_24h/counters/4xx", Some(json!(1))),
            "/metrics/window_24h/counters/4xx",
            |r| matches!(r, Rule::Pattern(Pattern::Key)),
        ),
        (
            "counter text",
            with(
                daily(),
                "/metrics/window_24h/counters/responses_4xx",
                Some(json!("1")),
            ),
            "/metrics/window_24h/counters/responses_4xx",
            |r| matches!(r, Rule::Type(Kind::Integer)),
        ),
    ]
}

#[test]
fn each_refusal_names_the_member_and_the_rule() {
    let validator = validator();
    for (name, report, member, rule) in refusals() {
        assert!(
            !validator.is_valid(&report),
            "{name}: the schema refuses it too"
        );
        let refusal = read(&body(&report)).expect_err(name);
        assert_eq!(member, refusal.member(), "{name}: {refusal}");
        assert!(rule(refusal.rule()), "{name}: {refusal}");
        assert!(refusal.to_string().starts_with(member), "{name}: {refusal}");
    }
}

#[test]
fn a_start_report_carries_first_start_and_no_metrics() {
    let mut start = with(daily(), "/metrics", None);
    start = with(start, "/event", Some(json!("start")));
    let without = read(&body(&start)).expect_err("first_start is required");
    assert_eq!("/first_start", without.member());
    let start = with(start, "/first_start", Some(json!(false)));
    assert!(
        matches!(
            read(&body(&start)).expect("the start reads").event,
            Event::Start { first_start: false }
        ),
        "a start report"
    );
    let with_metrics = with(start, "/metrics", daily().get("metrics").cloned());
    let refusal = read(&body(&with_metrics)).expect_err("metrics on a start");
    assert_eq!("/metrics", refusal.member());
    assert!(matches!(refusal.rule(), Rule::OtherEvent), "{refusal}");
}

#[test]
fn the_bounds_admit_their_edge_and_refuse_one_past_it() {
    let validator = validator();
    let members = |n: usize, value: Value| {
        Value::Object((0..n).map(|i| (format!("k{i}"), value.clone())).collect())
    };
    let cases = [
        (
            "/status",
            members(32, json!(true)),
            members(33, json!(true)),
        ),
        (
            "/metrics/window_24h/counters",
            members(64, json!(1)),
            members(65, json!(1)),
        ),
        (
            "/status/lanes",
            json!((0..16).map(|i| format!("t{i}")).collect::<Vec<_>>()),
            json!((0..17).map(|i| format!("t{i}")).collect::<Vec<_>>()),
        ),
        ("/status/domains", json!(10_000_000), json!(10_000_001)),
        (
            "/metrics/window_24h/groups/pdqm_search/p50_ms",
            json!(600_000.0),
            json!(600_000.5),
        ),
    ];
    for (pointer, edge, past) in cases {
        let admitted = with(daily(), pointer, Some(edge));
        assert!(
            validator.is_valid(&admitted),
            "{pointer}: the schema admits the edge"
        );
        assert!(read(&body(&admitted)).is_ok(), "{pointer}: the edge reads");
        let refused = with(daily(), pointer, Some(past));
        assert!(
            !validator.is_valid(&refused),
            "{pointer}: the schema refuses past it"
        );
        assert_eq!(
            pointer,
            read(&body(&refused)).expect_err(pointer).member(),
            "{pointer}"
        );
    }
}

#[test]
fn a_whole_number_written_with_a_fraction_is_an_integer_as_the_schema_counts_it() {
    let validator = validator();
    let report = with(
        with(daily(), "/schema", Some(json!(2.0))),
        "/uptime_s",
        Some(json!(1e3)),
    );
    assert!(validator.is_valid(&report), "the schema admits 2.0 and 1e3");
    assert_eq!(
        1000,
        read(&body(&report)).expect("the report reads").uptime_s
    );
}

#[test]
fn a_repeated_member_and_broken_text_are_refused() {
    let repeated = br#"{"schema": 2, "schema": 2}"#;
    let refusal = read(repeated).expect_err("a repeated member");
    assert_eq!("/schema", refusal.member());
    assert!(matches!(refusal.rule(), Rule::Repeated), "{refusal}");
    let refusal = read(b"[1, 2").expect_err("no JSON");
    assert!(matches!(refusal.rule(), Rule::Json(_)), "{refusal}");
    assert!(refusal.to_string().starts_with("the report"), "{refusal}");
    let refusal = read(b"[]").expect_err("no object");
    assert!(
        matches!(refusal.rule(), Rule::Type(Kind::Object)),
        "{refusal}"
    );
}
