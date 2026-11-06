// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A report built in memory, for the tests of the modules that read one.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::fingerprint::Fingerprint;
use crate::histogram;
use crate::report::{
    GroupReport, IdleReport, LoadReport, PlanIdentity, Report, SCHEMA, TargetInfo,
};

/// A load phase whose one group, `read`, answered `n` requests per half at
/// `latency_ms`, over a measured window of ten seconds.
pub(crate) fn load(latency_ms: u64, n: u64) -> LoadReport {
    let mut half = histogram::empty();
    for _ in 0..n {
        histogram::record(&mut half, Duration::from_millis(latency_ms));
    }
    LoadReport {
        measured_secs: 10.0,
        planned: 2 * n,
        sent: 2 * n,
        saturated: false,
        lag_p99_ms: Some(0.5),
        max_lag_ms: 10,
        peak_memory_bytes: Some(100_000_000),
        cpu_ms: Some(2.0 * 100.0 * 0.5),
        transactions: Some(2 * n),
        groups: BTreeMap::from([(
            "read".to_owned(),
            GroupReport::new(2 * n, &half, &half).unwrap(),
        )]),
        failures: BTreeMap::new(),
    }
}

/// A quiet run of [`load`] at 10 ms over 250 requests per half.
pub(crate) fn report() -> Report {
    Report {
        schema: SCHEMA,
        tool: "0.0.0".to_owned(),
        started: "2026-11-06T10:00:00Z".to_owned(),
        finished: "2026-11-06T10:02:00Z".to_owned(),
        host: Fingerprint {
            arch: "x86_64".to_owned(),
            os: "linux".to_owned(),
            available_parallelism: Some(4),
            cpu_model: None,
            total_memory_bytes: None,
        },
        target: TargetInfo {
            process: "process 1".to_owned(),
            base_url: "http://127.0.0.1:8080".to_owned(),
            database: Some("app".to_owned()),
        },
        plan: PlanIdentity {
            rate: 50.0,
            warmup_secs: 2,
            duration_secs: 10,
            requests: 1,
            groups: vec!["read".to_owned()],
        },
        idle: IdleReport {
            duration_secs: 60.0,
            cpu_percent: Some(0.3),
            max_cpu_percent: 2.0,
            transactions_per_second: Some(0.1),
            max_transactions_per_second: 5.0,
            memory_start_bytes: Some(90_000_000),
            memory_end_bytes: Some(90_000_000),
        },
        load: load(10, 250),
        calibration: None,
    }
}
