// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The committed baseline, `perf/baseline.toml`: the load metrics a run is
//! compared against, their tolerances and the A/A noise floor.
//!
//! The file names its `schema`. A reader migrates every older schema to the
//! current one before it reads the values, so a schema bump never makes a
//! committed baseline unreadable; a newer schema is refused by name.
//! Updating is a ratchet: on the same plan each metric moves only to the
//! better of the baseline and the run, so a run never loosens the baseline,
//! and a raise is a hand edit in a change that says why. The tolerances are
//! the product's and survive every update. No specification governs the
//! format: our own design.
//!
//! ```toml
//! schema = 1
//! tool = "0.1.0"
//! written = "2026-11-06T10:00:00Z"
//!
//! [plan]
//! rate = 50.0
//! warmup_secs = 10
//! duration_secs = 60
//! requests = 2
//! groups = ["create", "metadata"]
//!
//! [host]
//! arch = "x86_64"
//! os = "linux"
//!
//! [tolerance]
//! latency = 0.25
//!
//! [metrics]
//! throughput_rps = 49.9
//! "metadata.p95_ms" = 4.1
//!
//! [noise]
//! "metadata.p95_ms" = 1.08
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fingerprint::Fingerprint;
use crate::metric::{Kind, Metric};
use crate::report::PlanIdentity;

/// The schema this version writes.
pub const SCHEMA: i64 = 1;

/// A baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The schema, [`SCHEMA`].
    pub schema: i64,
    /// The version of `ferrohealth-perf` that wrote it.
    pub tool: String,
    /// When it was written, RFC 3339 in UTC.
    pub written: String,
    /// The plan it was measured with.
    pub plan: PlanIdentity,
    /// The host it was measured on.
    pub host: Fingerprint,
    /// The tolerance of each kind, as a fraction.
    #[serde(default)]
    pub tolerance: Tolerances,
    /// The value of each metric.
    pub metrics: BTreeMap<Metric, f64>,
    /// The A/A noise floor of each metric, a ratio of at least one.
    #[serde(default)]
    pub noise: BTreeMap<Metric, f64>,
}

/// The tolerance of each metric kind, as a fraction of the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tolerances {
    /// Latency quantiles.
    pub latency: f64,
    /// Throughput, which may fall by this share.
    pub throughput: f64,
    /// Peak memory.
    pub memory: f64,
    /// CPU per request.
    pub cpu: f64,
    /// Transactions per request.
    pub transactions: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Self {
            latency: Kind::Latency.default_tolerance(),
            throughput: Kind::Throughput.default_tolerance(),
            memory: Kind::Memory.default_tolerance(),
            cpu: Kind::Cpu.default_tolerance(),
            transactions: Kind::Transactions.default_tolerance(),
        }
    }
}

impl Tolerances {
    /// The tolerance of `kind`.
    #[must_use]
    pub fn of(&self, kind: Kind) -> f64 {
        match kind {
            Kind::Latency => self.latency,
            Kind::Throughput => self.throughput,
            Kind::Memory => self.memory,
            Kind::Cpu => self.cpu,
            Kind::Transactions => self.transactions,
        }
    }
}

/// A baseline that cannot be read or written.
#[derive(Debug, Error)]
pub enum BaselineError {
    /// The file could not be read or written.
    #[error("cannot access the baseline {path}")]
    Io {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a baseline.
    #[error("the baseline does not parse")]
    Parse(#[source] toml::de::Error),
    /// The file names no schema.
    #[error("the baseline names no integer schema")]
    NoSchema,
    /// The file was written by a newer version.
    #[error("the baseline has schema {0}; this ferrohealth-perf reads schema {SCHEMA} and older")]
    Schema(i64),
    /// A value the file holds is refused.
    #[error("the baseline is refused: {0}")]
    Invalid(String),
    /// The baseline does not serialize.
    #[error("the baseline does not serialize")]
    Serialize(#[source] toml::ser::Error),
}

impl Baseline {
    /// Reads the baseline at `path`; `None` when there is no file.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Io`] when the file exists and cannot be read, and
    /// every error of [`Baseline::parse`].
    pub fn read(path: &Path) -> Result<Option<Self>, BaselineError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(BaselineError::Io {
                path: path.display().to_string(),
                source,
            }),
        }
    }

    /// Parses a baseline, migrating an older schema first.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Parse`] for text that is not TOML or not a baseline,
    /// [`BaselineError::NoSchema`] and [`BaselineError::Schema`] for a
    /// missing or newer schema, and [`BaselineError::Invalid`] for a
    /// negative or non-finite value or a noise floor below one.
    pub fn parse(text: &str) -> Result<Self, BaselineError> {
        let table: toml::Table = toml::from_str(text).map_err(BaselineError::Parse)?;
        let schema = table
            .get("schema")
            .and_then(toml::Value::as_integer)
            .ok_or(BaselineError::NoSchema)?;
        let table = migrate(schema, table)?;
        let baseline: Self = table.try_into().map_err(BaselineError::Parse)?;
        baseline.check()?;
        Ok(baseline)
    }

    fn check(&self) -> Result<(), BaselineError> {
        let finite = |value: f64| value.is_finite() && value >= 0.0;
        let t = &self.tolerance;
        for (kind, value) in [
            ("latency", t.latency),
            ("throughput", t.throughput),
            ("memory", t.memory),
            ("cpu", t.cpu),
            ("transactions", t.transactions),
        ] {
            if !finite(value) {
                return Err(BaselineError::Invalid(format!(
                    "tolerance.{kind} is {value}"
                )));
            }
        }
        if t.throughput >= 1.0 {
            return Err(BaselineError::Invalid(
                "tolerance.throughput must be below 1".to_owned(),
            ));
        }
        if let Some((metric, value)) = self.metrics.iter().find(|(_, v)| !finite(**v)) {
            return Err(BaselineError::Invalid(format!(
                "metrics.{metric} is {value}"
            )));
        }
        if let Some((metric, value)) = self
            .noise
            .iter()
            .find(|(_, v)| !(v.is_finite() && **v >= 1.0))
        {
            return Err(BaselineError::Invalid(format!(
                "noise.{metric} is {value}, below one"
            )));
        }
        Ok(())
    }

    /// The baseline as TOML.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Serialize`] if it does not serialize.
    pub fn to_toml(&self) -> Result<String, BaselineError> {
        let body = toml::to_string(self).map_err(BaselineError::Serialize)?;
        Ok(format!(
            "# The performance baseline of ferrohealth-perf. `ferrohealth-perf run --update`\n\
             # lowers it; a raise is a hand edit in a change that says why.\n{body}"
        ))
    }

    /// Writes the baseline to `path`, through a rename.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Serialize`] or [`BaselineError::Io`].
    pub fn write(&self, path: &Path) -> Result<(), BaselineError> {
        let text = self.to_toml()?;
        let io = |source| BaselineError::Io {
            path: path.display().to_string(),
            source,
        };
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let partial = path.with_extension("toml.partial");
        std::fs::write(&partial, text).map_err(io)?;
        std::fs::rename(&partial, path).map_err(io)
    }
}

/// Brings a baseline of `schema` to [`SCHEMA`]. Schema 1 is the first, so
/// today every older schema is unknown; a bump adds its step here.
fn migrate(schema: i64, table: toml::Table) -> Result<toml::Table, BaselineError> {
    match schema {
        SCHEMA => Ok(table),
        other => Err(BaselineError::Schema(other)),
    }
}

/// What an update did to one metric.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The metric is new to the baseline.
    Added(Metric, f64),
    /// The run was better, so the baseline moved to it.
    Tightened {
        /// The metric.
        metric: Metric,
        /// The baseline before.
        from: f64,
        /// The baseline after.
        to: f64,
    },
    /// The run was not better, so the baseline kept its value.
    Kept(Metric, f64),
    /// The plan changed, so the baseline was written afresh.
    Rewritten,
}

/// The inputs of an update besides the old baseline.
#[derive(Debug, Clone)]
pub struct Update<'a> {
    /// The run's metrics.
    pub metrics: &'a BTreeMap<Metric, f64>,
    /// The A/A noise floor measured with it.
    pub noise: BTreeMap<Metric, f64>,
    /// The run's plan.
    pub plan: &'a PlanIdentity,
    /// The run's host.
    pub host: &'a Fingerprint,
    /// The version writing it.
    pub tool: &'a str,
    /// When it is written.
    pub written: &'a str,
}

/// The baseline an update writes, and what it did to each metric.
///
/// With no old baseline, or one of another plan, the run's metrics become
/// the baseline. On the same plan each metric moves to the better of the
/// two values and a metric new to the run is added. The tolerances of the
/// old baseline are kept.
#[must_use]
pub fn updated(old: Option<&Baseline>, update: &Update<'_>) -> (Baseline, Vec<Change>) {
    let tolerance = old.map(|b| b.tolerance).unwrap_or_default();
    let fresh = |changes| {
        (
            Baseline {
                schema: SCHEMA,
                tool: update.tool.to_owned(),
                written: update.written.to_owned(),
                plan: update.plan.clone(),
                host: update.host.clone(),
                tolerance,
                metrics: update.metrics.clone(),
                noise: update.noise.clone(),
            },
            changes,
        )
    };
    let Some(old) = old.filter(|b| &b.plan == update.plan) else {
        let changes = if old.is_some() {
            vec![Change::Rewritten]
        } else {
            update
                .metrics
                .iter()
                .map(|(metric, value)| Change::Added(metric.clone(), *value))
                .collect()
        };
        return fresh(changes);
    };
    let mut metrics = BTreeMap::new();
    let mut changes = Vec::new();
    for (metric, run) in update.metrics {
        let Some(base) = old.metrics.get(metric).copied() else {
            metrics.insert(metric.clone(), *run);
            changes.push(Change::Added(metric.clone(), *run));
            continue;
        };
        let better = if metric.kind().lower_is_worse() {
            *run > base
        } else {
            *run < base
        };
        if better {
            metrics.insert(metric.clone(), *run);
            changes.push(Change::Tightened {
                metric: metric.clone(),
                from: base,
                to: *run,
            });
        } else {
            metrics.insert(metric.clone(), base);
            changes.push(Change::Kept(metric.clone(), base));
        }
    }
    let (mut baseline, _) = fresh(Vec::new());
    baseline.metrics = metrics;
    (baseline, changes)
}

/// The A/A noise floor of two runs of one plan: per metric, the larger of
/// the two ratios between them; a metric that is zero in either is left
/// out.
#[must_use]
pub fn noise_floor(a: &BTreeMap<Metric, f64>, b: &BTreeMap<Metric, f64>) -> BTreeMap<Metric, f64> {
    a.iter()
        .filter_map(|(metric, x)| {
            let y = *b.get(metric)?;
            (*x > 0.0 && y > 0.0).then(|| (metric.clone(), (x / y).max(y / x)))
        })
        .collect()
}

#[cfg(test)]
mod tests;
