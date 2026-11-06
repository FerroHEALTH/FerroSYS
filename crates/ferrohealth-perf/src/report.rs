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
pub const SCHEMA: u32 = 1;

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
    /// The second load phase of an A/A calibration, when the run wrote a
    /// baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<LoadReport>,
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
    #[error("the report has schema {0}; this ferrohealth-perf reads schema {SCHEMA}")]
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
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
            path: shown.clone(),
            source,
        })?;
        let report: Self = serde_json::from_str(&text).map_err(|source| ReportError::Parse {
            path: shown,
            source,
        })?;
        if report.schema != SCHEMA {
            return Err(ReportError::Schema(report.schema));
        }
        Ok(report)
    }

    /// Writes the report into `dir` as `report-<stamp>.json`, then replaces
    /// `latest.json` with a copy through a rename; returns the dated path.
    ///
    /// # Errors
    ///
    /// [`ReportError::Io`] when the directory or a file cannot be written,
    /// and [`ReportError::Parse`] if the report does not serialize.
    pub fn write(&self, dir: &Path, stamp: &str) -> Result<PathBuf, ReportError> {
        let io = |path: &Path| {
            let path = path.display().to_string();
            move |source| ReportError::Io { path, source }
        };
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        let text = serde_json::to_string_pretty(self).map_err(|source| ReportError::Parse {
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
}
