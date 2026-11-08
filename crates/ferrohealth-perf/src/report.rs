// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The JSON report of a run, and the files it is written to.
//!
//! The report holds every measurement and each group's latency histograms,
//! encoded, so [`crate::metric::of`] re-derives every compared value from
//! the report alone and a saved report can be judged again offline. A run
//! writes `report-<UTC time>.json` and then replaces `latest.json` through
//! a rename, so a reader never sees half a file. No specification governs
//! the format: our own design.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fingerprint::Fingerprint;
use crate::histogram::{self, HistogramError};
use crate::quantile::{Quantile, QuantileName, quantile};

/// The schema of the report this version writes and reads.
pub const SCHEMA: u32 = 3;

/// The band within which the second half's p50 and p95 must lie, as a
/// share of the first half's.
pub const STEADY: (f64, f64) = (0.8, 1.25);

/// One run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// The report schema, [`SCHEMA`].
    pub schema: u32,
    /// The version of `ferrohealth-perf` that wrote it.
    pub tool: String,
    /// When the run started, RFC 3339 in UTC.
    pub started: String,
    /// When the run finished, RFC 3339 in UTC.
    pub finished: String,
    /// The host the run was measured on.
    pub host: Fingerprint,
    /// The server measured.
    pub target: TargetInfo,
    /// The plan the run followed.
    pub plan: PlanIdentity,
    /// The idle phase.
    pub idle: IdleReport,
    /// The load phase.
    pub load: LoadReport,
    /// The further load phases of an update, each after its own warm-up,
    /// whose medians and spread with [`Report::load`] write the baseline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calibration: Vec<LoadReport>,
    /// The fresh load phases a breach of [`Report::load`] was confirmed
    /// or cleared on, each after its own warm-up; at most two.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmations: Vec<LoadReport>,
}

/// The server a run measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetInfo {
    /// The process or cgroup read.
    pub process: String,
    /// The base URL, its credentials redacted.
    pub base_url: String,
    /// The database whose transactions were counted, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// The MLLP listener the plan's HL7 v2 messages went to, when it sends
    /// any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mllp: Option<String>,
}

/// What makes two runs comparable: the load and the request set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanIdentity {
    /// The arrivals per second.
    pub rate: f64,
    /// The warm-up, in seconds.
    pub warmup_secs: u64,
    /// The measured window, in seconds.
    pub duration_secs: u64,
    /// The requests in the set.
    pub requests: u64,
    /// The request groups, sorted.
    pub groups: Vec<String>,
}

/// The idle phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdleReport {
    /// How long it measured, in seconds.
    pub duration_secs: f64,
    /// The server's CPU time as a percentage of one core, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// The limit on it.
    pub max_cpu_percent: f64,
    /// The database's transactions per second, when a database was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transactions_per_second: Option<f64>,
    /// The limit on them.
    pub max_transactions_per_second: f64,
    /// The server's memory at the start, in bytes, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_start_bytes: Option<u64>,
    /// The server's memory at the end, in bytes, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_end_bytes: Option<u64>,
}

/// The load phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadReport {
    /// The measured window, in seconds.
    pub measured_secs: f64,
    /// The arrivals the schedule held, warm-up included.
    pub planned: u64,
    /// The arrivals sent.
    pub sent: u64,
    /// Whether an arrival waited past the saturation limit.
    pub saturated: bool,
    /// The p99 of the generator's lag over the measured window, in
    /// milliseconds, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_p99_ms: Option<f64>,
    /// The limit on that lag, in milliseconds.
    pub max_lag_ms: u64,
    /// The ratio of p99 to p50 above which every group at once marks a
    /// stalled runner; absent in a report written before 0.4.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_ratio: Option<f64>,
    /// The server's peak memory over the phase, in bytes, when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_memory_bytes: Option<u64>,
    /// The server's CPU time over the measured window, in milliseconds,
    /// when readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<f64>,
    /// The database's transactions over the whole phase, when a database
    /// was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transactions: Option<u64>,
    /// Each group, by name.
    pub groups: BTreeMap<String, GroupReport>,
    /// The failures over the whole phase, by group and kind.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failures: BTreeMap<String, BTreeMap<String, u64>>,
}

/// One group of the load phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupReport {
    /// The requests answered as expected in the measured window.
    pub answered: u64,
    /// The quantiles of the whole window, for a reader; a comparison
    /// derives them again from the histograms.
    pub quantiles: BTreeMap<QuantileName, Quantile>,
    /// The latencies of the first half, in microseconds, V2-encoded.
    pub first_half: String,
    /// The latencies of the second half, in microseconds, V2-encoded.
    pub second_half: String,
}

/// The second half's p50 and p95 over the first's.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Steadiness {
    /// The p50 ratio, when both halves resolve it.
    pub p50_ratio: Option<f64>,
    /// The p95 ratio, when both halves resolve it.
    pub p95_ratio: Option<f64>,
}

impl Steadiness {
    /// Whether every ratio that exists lies within [`STEADY`], and one does.
    #[must_use]
    pub fn steady(&self) -> bool {
        let ratios: Vec<f64> = [self.p50_ratio, self.p95_ratio]
            .into_iter()
            .flatten()
            .collect();
        !ratios.is_empty() && ratios.iter().all(|r| (STEADY.0..=STEADY.1).contains(r))
    }
}

/// A report that cannot be read, derived from or written.
#[derive(Debug, Error)]
pub enum ReportError {
    /// A histogram of the report does not decode or merge.
    #[error(transparent)]
    Histogram(#[from] HistogramError),
    /// The file could not be read or written.
    #[error("cannot access the report {path}")]
    Io {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a report.
    #[error("the report {path} does not parse")]
    Parse {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: serde_json::Error,
    },
    /// The report was written by a schema this version does not read.
    #[error("the report has schema {0}; this ferrohealth-perf reads schemas 1 to {SCHEMA}")]
    Schema(u32),
}

impl GroupReport {
    /// Builds a group from its two halves.
    ///
    /// # Errors
    ///
    /// [`ReportError::Histogram`] when a half cannot be encoded or merged.
    pub fn new(
        answered: u64,
        first: &Histogram<u64>,
        second: &Histogram<u64>,
    ) -> Result<Self, ReportError> {
        let mut whole = first.clone();
        histogram::merge(&mut whole, second)?;
        Ok(Self {
            answered,
            quantiles: QuantileName::ALL
                .into_iter()
                .map(|name| (name, quantile(&whole, name)))
                .collect(),
            first_half: histogram::encode(first)?,
            second_half: histogram::encode(second)?,
        })
    }

    /// The two halves, decoded.
    ///
    /// # Errors
    ///
    /// [`ReportError::Histogram`] when one does not decode.
    pub fn halves(&self) -> Result<(Histogram<u64>, Histogram<u64>), ReportError> {
        Ok((
            histogram::decode(&self.first_half)?,
            histogram::decode(&self.second_half)?,
        ))
    }
}

impl LoadReport {
    /// The merged histograms of every group, by half.
    ///
    /// # Errors
    ///
    /// [`ReportError::Histogram`] when one does not decode or merge.
    pub fn overall_halves(&self) -> Result<(Histogram<u64>, Histogram<u64>), ReportError> {
        let mut first = histogram::empty();
        let mut second = histogram::empty();
        for group in self.groups.values() {
            let (a, b) = group.halves()?;
            histogram::merge(&mut first, &a)?;
            histogram::merge(&mut second, &b)?;
        }
        Ok((first, second))
    }

    /// The steadiness of the measured window, from the merged halves.
    ///
    /// # Errors
    ///
    /// [`ReportError::Histogram`] when a histogram does not decode.
    pub fn steadiness(&self) -> Result<Steadiness, ReportError> {
        let (first, second) = self.overall_halves()?;
        let ratio = |name| {
            let a = quantile(&first, name).value_ms()?;
            let b = quantile(&second, name).value_ms()?;
            (a > 0.0).then_some(b / a)
        };
        Ok(Steadiness {
            p50_ratio: ratio(QuantileName::P50),
            p95_ratio: ratio(QuantileName::P95),
        })
    }

    /// Whether the runner stalled: every group whose p50 and p99 resolve
    /// has a p99 above [`LoadReport::stall_ratio`] times its p50, at once.
    /// A stall that slows only some routes is left to the confirmation of a
    /// breach.
    #[must_use]
    pub fn stalled(&self) -> bool {
        let Some(limit) = self.stall_ratio else {
            return false;
        };
        let ratios: Vec<f64> = self
            .groups
            .values()
            .filter_map(|group| {
                let at = |name| group.quantiles.get(&name).and_then(Quantile::value_ms);
                let (p50, p99) = (at(QuantileName::P50)?, at(QuantileName::P99)?);
                (p50 > 0.0).then_some(p99 / p50)
            })
            .collect();
        !ratios.is_empty() && ratios.iter().all(|ratio| *ratio > limit)
    }

    /// The requests answered as expected in the measured window.
    #[must_use]
    pub fn answered(&self) -> u64 {
        self.groups
            .values()
            .fold(0, |sum, group| sum.saturating_add(group.answered))
    }
}

impl Report {
    /// Reads a report from `path`.
    ///
    /// # Errors
    ///
    /// [`ReportError::Io`], [`ReportError::Parse`] or [`ReportError::Schema`].
    pub fn read(path: &Path) -> Result<Self, ReportError> {
        let (mut value, parse) = read_json(path)?;
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0);
        match schema {
            SCHEMA => {}
            2 => {
                if let Some(object) = value.as_object_mut() {
                    object.insert("schema".to_owned(), serde_json::Value::from(SCHEMA));
                }
            }
            // NOTE: no specification governs this: our own design; schema 1 held one calibration phase, not a list.
            1 => {
                if let Some(object) = value.as_object_mut() {
                    if let Some(one) = object.remove("calibration") {
                        object.insert(
                            "calibration".to_owned(),
                            serde_json::Value::Array(vec![one]),
                        );
                    }
                    object.insert("schema".to_owned(), serde_json::Value::from(SCHEMA));
                }
            }
            other => return Err(ReportError::Schema(other)),
        }
        serde_json::from_value(value).map_err(parse)
    }
}

/// The JSON of the report file at `path`, and the error a value of it that
/// does not deserialize is reported as.
pub(crate) fn read_json(
    path: &Path,
) -> Result<(serde_json::Value, impl Fn(serde_json::Error) -> ReportError), ReportError> {
    let shown = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: shown.clone(),
        source,
    })?;
    let parse = move |source| ReportError::Parse {
        path: shown.clone(),
        source,
    };
    let value = serde_json::from_str(&text).map_err(&parse)?;
    Ok((value, parse))
}

/// Writes `report` into `dir` as `report-<stamp>.json`, then replaces
/// `latest.json` with a copy through a rename, so a reader never sees half a
/// file; returns the dated path.
///
/// # Errors
///
/// [`ReportError::Io`] when the directory or a file cannot be written, and
/// [`ReportError::Parse`] if the report does not serialize.
pub fn write<T: Serialize>(report: &T, dir: &Path, stamp: &str) -> Result<PathBuf, ReportError> {
    let io = |path: &Path| {
        let path = path.display().to_string();
        move |source| ReportError::Io { path, source }
    };
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    let text = serde_json::to_string_pretty(report).map_err(|source| ReportError::Parse {
        path: dir.display().to_string(),
        source,
    })?;
    let dated = dir.join(format!("report-{stamp}.json"));
    std::fs::write(&dated, &text).map_err(io(&dated))?;
    let partial = dir.join(".latest.json.partial");
    std::fs::write(&partial, &text).map_err(io(&partial))?;
    let latest = dir.join("latest.json");
    std::fs::rename(&partial, &latest).map_err(io(&latest))?;
    Ok(dated)
}

#[cfg(test)]
pub(crate) mod fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn halves(first_ms: u64, second_ms: u64, n: u64) -> (Histogram<u64>, Histogram<u64>) {
        let mut a = histogram::empty();
        let mut b = histogram::empty();
        for _ in 0..n {
            histogram::record(&mut a, Duration::from_millis(first_ms));
            histogram::record(&mut b, Duration::from_millis(second_ms));
        }
        (a, b)
    }

    fn load_of(first_ms: u64, second_ms: u64) -> LoadReport {
        let (a, b) = halves(first_ms, second_ms, 200);
        LoadReport {
            measured_secs: 4.0,
            planned: 400,
            sent: 400,
            saturated: false,
            lag_p99_ms: Some(0.5),
            max_lag_ms: 10,
            stall_ratio: Some(10.0),
            peak_memory_bytes: None,
            cpu_ms: None,
            transactions: None,
            groups: BTreeMap::from([("read".to_owned(), GroupReport::new(400, &a, &b).unwrap())]),
            failures: BTreeMap::new(),
        }
    }

    #[test]
    fn a_window_whose_halves_agree_is_steady() {
        let steadiness = load_of(10, 11).steadiness().unwrap();
        assert!(steadiness.steady(), "{steadiness:?}");
    }

    #[test]
    fn a_window_whose_second_half_slows_is_unsteady() {
        let steadiness = load_of(10, 20).steadiness().unwrap();
        assert!(!steadiness.steady(), "{steadiness:?}");
        assert!(steadiness.p50_ratio.unwrap() > 1.9, "{steadiness:?}");
    }

    #[test]
    fn a_window_of_too_few_samples_is_not_steady() {
        let (a, b) = halves(10, 10, 1);
        let mut load = load_of(10, 10);
        load.groups = BTreeMap::from([("read".to_owned(), GroupReport::new(2, &a, &b).unwrap())]);
        assert!(!load.steadiness().unwrap().steady(), "nothing to compare");
    }

    #[test]
    fn a_group_keeps_its_quantiles_and_its_halves() {
        let load = load_of(10, 10);
        let group = load.groups.get("read").unwrap();
        let p50 = group
            .quantiles
            .get(&QuantileName::P50)
            .unwrap()
            .value_ms()
            .unwrap();
        assert!((p50 - 10.0).abs() < 0.05, "{p50}");
        let (a, b) = group.halves().unwrap();
        assert_eq!((200, 200), (a.len(), b.len()));
        assert_eq!(400, load.answered());
    }

    #[test]
    fn a_schema_1_report_reads_with_its_one_calibration_as_a_list() {
        let mut report = fixture::report();
        report.calibration = vec![load_of(10, 10)];
        let mut value = serde_json::to_value(&report).unwrap();
        let object = value.as_object_mut().unwrap();
        let one = object
            .get_mut("calibration")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .remove(0);
        object.insert("calibration".to_owned(), one);
        object.insert("schema".to_owned(), serde_json::Value::from(1));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.json");
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
        let read = Report::read(&path).unwrap();
        assert_eq!(SCHEMA, read.schema);
        assert_eq!(report.calibration, read.calibration);
        std::fs::write(&path, "{\"schema\": 9}").unwrap();
        assert!(matches!(Report::read(&path), Err(ReportError::Schema(9))));
    }

    #[test]
    fn a_stall_is_every_route_at_once_past_the_ratio() {
        let mut load = load_of(10, 10);
        assert!(!load.stalled(), "a steady phase");
        load = fixture::tailed(10, 500, 1000);
        assert!(load.stalled(), "p99 at 50 times p50");
        let quiet = load_of(10, 10).groups.remove("read").unwrap();
        load.groups.insert("other".to_owned(), quiet);
        assert!(
            !load.stalled(),
            "one route held, so it is no whole-host stall"
        );
        load.stall_ratio = None;
        assert!(!load.stalled(), "a report written before 0.4.0");
    }
}
