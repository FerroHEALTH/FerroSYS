// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The load metrics a baseline holds, each with its kind, its direction and
//! its default tolerance.
//!
//! A metric is named in a baseline by its key: `throughput_rps`,
//! `peak_memory_bytes`, `cpu_per_request_ms`, `transactions_per_request`,
//! or `<group>.<quantile>_ms` for a group's latency. No specification
//! governs the set: the issue's own list (#249).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::histogram;
use crate::quantile::{QuantileName, exact_f64, quantile};
use crate::report::{LoadReport, ReportError};

/// One compared metric of the load phase.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Metric {
    /// Requests answered as expected per second of the measured window.
    Throughput,
    /// The server's peak memory over the phase, in bytes.
    PeakMemory,
    /// The server's CPU time per answered request, in milliseconds.
    CpuPerRequest,
    /// The database's transactions per request sent.
    TransactionsPerRequest,
    /// One latency quantile of one group, in milliseconds.
    Latency {
        /// The group.
        group: String,
        /// The quantile.
        quantile: QuantileName,
    },
}

/// The kind of a metric, which sets its direction and its tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A latency quantile.
    Latency,
    /// Throughput, where lower is worse.
    Throughput,
    /// Memory.
    Memory,
    /// CPU time.
    Cpu,
    /// Database transactions.
    Transactions,
}

impl Kind {
    /// The tolerance a baseline takes when it names none: latency and CPU
    /// +25 %, throughput −20 %, memory +20 %, transactions +10 % (#249).
    #[must_use]
    pub fn default_tolerance(self) -> f64 {
        match self {
            Self::Latency | Self::Cpu => 0.25,
            Self::Throughput | Self::Memory => 0.20,
            Self::Transactions => 0.10,
        }
    }

    /// Whether a lower value is the worse one.
    #[must_use]
    pub fn lower_is_worse(self) -> bool {
        matches!(self, Self::Throughput)
    }
}

impl Metric {
    /// The metric's kind.
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Self::Throughput => Kind::Throughput,
            Self::PeakMemory => Kind::Memory,
            Self::CpuPerRequest => Kind::Cpu,
            Self::TransactionsPerRequest => Kind::Transactions,
            Self::Latency { .. } => Kind::Latency,
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Throughput => f.write_str("throughput_rps"),
            Self::PeakMemory => f.write_str("peak_memory_bytes"),
            Self::CpuPerRequest => f.write_str("cpu_per_request_ms"),
            Self::TransactionsPerRequest => f.write_str("transactions_per_request"),
            Self::Latency { group, quantile } => write!(f, "{group}.{}_ms", quantile.token()),
        }
    }
}

impl From<Metric> for String {
    fn from(metric: Metric) -> Self {
        metric.to_string()
    }
}

impl TryFrom<String> for Metric {
    type Error = String;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        match key.as_str() {
            "throughput_rps" => return Ok(Self::Throughput),
            "peak_memory_bytes" => return Ok(Self::PeakMemory),
            "cpu_per_request_ms" => return Ok(Self::CpuPerRequest),
            "transactions_per_request" => return Ok(Self::TransactionsPerRequest),
            _ => {}
        }
        let unknown = || format!("unknown metric {key:?}");
        let (group, rest) = key.split_once('.').ok_or_else(unknown)?;
        let quantile = QuantileName::ALL
            .into_iter()
            .find(|name| rest == format!("{}_ms", name.token()))
            .ok_or_else(unknown)?;
        if group.is_empty() {
            return Err(unknown());
        }
        Ok(Self::Latency {
            group: group.to_owned(),
            quantile,
        })
    }
}

/// Every compared value of `load`, by metric, derived from its histograms
/// and counts; a value that cannot be measured is absent.
///
/// # Errors
///
/// [`ReportError::Histogram`] when a histogram does not decode or merge.
pub fn of(load: &LoadReport) -> Result<BTreeMap<Metric, f64>, ReportError> {
    let mut metrics = BTreeMap::new();
    let answered = load.answered();
    if load.measured_secs > 0.0 {
        metrics.insert(Metric::Throughput, exact_f64(answered) / load.measured_secs);
    }
    if let Some(bytes) = load.peak_memory_bytes {
        metrics.insert(Metric::PeakMemory, exact_f64(bytes));
    }
    if let (Some(cpu_ms), true) = (load.cpu_ms, answered > 0) {
        metrics.insert(Metric::CpuPerRequest, cpu_ms / exact_f64(answered));
    }
    if let (Some(transactions), true) = (load.transactions, load.sent > 0) {
        metrics.insert(
            Metric::TransactionsPerRequest,
            exact_f64(transactions) / exact_f64(load.sent),
        );
    }
    for (group, report) in &load.groups {
        let (mut whole, second) = report.halves()?;
        histogram::merge(&mut whole, &second)?;
        for name in QuantileName::ALL {
            if let Some(value) = quantile(&whole, name).value_ms() {
                metrics.insert(
                    Metric::Latency {
                        group: group.clone(),
                        quantile: name,
                    },
                    value,
                );
            }
        }
    }
    Ok(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_round_trips() {
        for metric in [
            Metric::Throughput,
            Metric::PeakMemory,
            Metric::CpuPerRequest,
            Metric::TransactionsPerRequest,
            Metric::Latency {
                group: "read-one".to_owned(),
                quantile: QuantileName::P95,
            },
        ] {
            assert_eq!(Ok(metric.clone()), Metric::try_from(metric.to_string()));
        }
        assert_eq!(
            "read.p99_ms",
            Metric::Latency {
                group: "read".to_owned(),
                quantile: QuantileName::P99
            }
            .to_string()
        );
    }

    #[test]
    fn an_unknown_key_is_refused() {
        for key in ["latency", "read.p90_ms", ".p50_ms", "read.p50"] {
            assert!(Metric::try_from(key.to_owned()).is_err(), "{key}");
        }
    }

    #[test]
    fn the_default_tolerances_are_the_issue_s() {
        assert!(
            (Kind::Latency.default_tolerance() - 0.25).abs() < f64::EPSILON,
            "latency"
        );
        assert!(
            (Kind::Cpu.default_tolerance() - 0.25).abs() < f64::EPSILON,
            "cpu"
        );
        assert!(
            (Kind::Throughput.default_tolerance() - 0.20).abs() < f64::EPSILON,
            "rps"
        );
        assert!(
            (Kind::Memory.default_tolerance() - 0.20).abs() < f64::EPSILON,
            "memory"
        );
        assert!(
            (Kind::Transactions.default_tolerance() - 0.10).abs() < f64::EPSILON,
            "transactions"
        );
        assert!(Kind::Throughput.lower_is_worse(), "throughput falls");
        assert!(!Kind::Latency.lower_is_worse(), "latency rises");
    }
}
