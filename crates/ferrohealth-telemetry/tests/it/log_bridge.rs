// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The process subscriber with its `log` bridge (#389): a `log` check that
//! the console filter answers false leaves nothing behind, so the next event
//! is written (#388), and `log` records reach the console as the filter
//! says. Only a process subscriber caches the callsite interest that lets an
//! event skip its own `enabled` question, so the case runs in a child
//! process of this test binary.

use std::process::Command;

use ferrohealth_telemetry::section::{Filter, Format, TelemetrySection};
use ferrohealth_telemetry::telemetry::{Console, Layers, Service, Telemetry};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt as _;

/// Names the console filter of the child half, and asks the child to run.
const CHILD: &str = "FERROHEALTH_TELEMETRY_LOG_BRIDGE_FILTER";

/// One request line, written from one callsite every time.
fn request_line(at: &str) {
    tracing::info!(at, "request line");
}

/// The child half: the console on stdout under the filter the parent names,
/// installed with the bridge, then the events and records the parent reads.
#[test]
#[ignore = "the child half of a_log_check_leaves_the_next_event_written, run in its own process"]
fn log_bridge_child() {
    let Ok(filter) = std::env::var(CHILD) else {
        return;
    };
    let section = TelemetrySection {
        format: Format::Pretty,
        filter: Filter::new(filter).expect("the filter parses"),
        ..TelemetrySection::default()
    };
    let telemetry = Telemetry::new(&section, Service::new("child", "0")).expect("telemetry");
    let Layers { layers, filter: _ } = telemetry
        .layers::<Registry, _>(
            &section,
            &Console::new(false, Some(std::ffi::OsStr::new("1"))),
            std::io::stdout,
        )
        .expect("layers");
    ferrohealth_telemetry::log_bridge::install(tracing_subscriber::registry().with(layers))
        .expect("the subscriber and the bridge install");
    // One request span holds them all, as the request log holds a request.
    tracing::info_span!("request").in_scope(|| {
        request_line("before-the-check");
        // The check answers from the global maximum, `debug` here, as sqlx asks it.
        let checked = log::log_enabled!(target: "sqlx::query", log::Level::Debug);
        request_line("after-the-check");
        tracing::info!(checked, "the check");
        log::info!(target: "bridged", "an info record of a bridged target");
        log::debug!(target: "quiet", "a debug record below the filter");
        log::debug!(target: "x", "a debug record the filter lets through");
        request_line("after-the-records");
    });
}

#[test]
fn a_log_check_leaves_the_next_event_written() {
    let output = Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "log_bridge::log_bridge_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "info,x=debug")
        .output()
        .expect("the child runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the child failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for written in [
        "at=\"before-the-check\"",
        "at=\"after-the-check\"",
        "an info record of a bridged target",
        "a debug record the filter lets through",
        "at=\"after-the-records\"",
    ] {
        assert!(
            stdout.contains(written),
            "{written} is not written:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("a debug record below the filter"),
        "a record below the filter is written:\n{stdout}"
    );
}
