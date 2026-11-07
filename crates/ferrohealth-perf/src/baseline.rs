// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The committed baseline, `perf/baseline.toml`: per CPU model, the load
//! metrics a run is compared against, their tolerances and the A/A noise
//! floor.
//!
//! Shared CI runners mix CPU generations, and a latency or a CPU time is
//! comparable only on a like CPU, so the file holds one `[[baseline]]`
//! entry per CPU model, keyed by the model string of the host fingerprint.
//! A run is judged against the entry for its own model, and `--update`
//! writes or tightens that entry alone. An entry is measured over several
//! load phases: each metric is their median, and its noise floor is how far
//! the worst of them strayed from that median, so a tail that moves from
//! run to run is judged against the movement it was seen to have. The file names its `schema`. A
//! reader migrates every older schema to the current one before it reads
//! the values, so a schema bump never makes a committed baseline
//! unreadable; a newer schema is refused by name. An entry of schema 2 or
//! older was measured within one run and is kept marked for re-measurement.
//! Updating is a ratchet: on the same plan each metric moves only to the
//! better of the entry and the median of the new update, so neither a run
//! nor one lucky phase loosens or tightens it past what the runs show, and a raise is a hand edit in a change
//! that says why. The tolerances are the product's and survive every
//! update. No specification governs the format: our own design.
//!
//! ```toml
//! schema = 3
//!
//! [[baseline]]
//! tool = "0.3.0"
//! written = "2026-11-07T10:00:00Z"
//!
//! [baseline.plan]
//! rate = 50.0
//! warmup_secs = 10
//! duration_secs = 60
//! requests = 2
//! groups = ["create", "metadata"]
//!
//! [baseline.host]
//! arch = "x86_64"
//! os = "linux"
//! cpu_model = "AMD EPYC 7763 64-Core Processor"
//!
//! [baseline.tolerance]
//! latency = 0.25
//!
//! [baseline.metrics]
//! throughput_rps = 49.9
//! "metadata.p95_ms" = 4.1
//!
//! [baseline.noise]
//! "metadata.p95_ms" = 1.31
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fingerprint::Fingerprint;
use crate::metric::{Kind, Metric};
use crate::report::PlanIdentity;

/// The schema this version writes.
pub const SCHEMA: i64 = 3;

/// The baseline file: one entry per CPU model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baselines {
    /// The schema, [`SCHEMA`].
    pub schema: i64,
    /// The entries, one per CPU model, in the order of their model.
    #[serde(default, rename = "baseline")]
    pub entries: Vec<Baseline>,
}

/// The baseline of one CPU model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// The version of `ferrohealth-perf` that wrote it.
    pub tool: String,
    /// When it was written, RFC 3339 in UTC.
    pub written: String,
    /// The plan it was measured with.
    pub plan: PlanIdentity,
    /// The host it was measured on, whose CPU model keys the entry.
    pub host: Fingerprint,
    /// The tolerance of each kind, as a fraction.
    #[serde(default)]
    pub tolerance: Tolerances,
    /// The value of each metric.
    pub metrics: BTreeMap<Metric, f64>,
    /// The noise floor of each metric across the update's load phases, a
    /// ratio of at least one.
    #[serde(default)]
    pub noise: BTreeMap<Metric, f64>,
    /// Whether the entry predates the noise measured across runs (schema 2
    /// and older) and must be measured again with `--update` before it
    /// judges a load phase.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remeasure: bool,
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

/// The name a message gives the CPU model `model`.
#[must_use]
pub fn model_name(model: Option<&str>) -> String {
    model.map_or_else(
        || "with no model name (the host disclosed none)".to_owned(),
        |name| format!("`{name}`"),
    )
}

impl Baselines {
    /// Reads the baseline file at `path`; `None` when there is no file.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Io`] when the file exists and cannot be read, and
    /// every error of [`Baselines::parse`].
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

    /// Parses a baseline file, migrating an older schema first.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Parse`] for text that is not TOML or not a baseline
    /// file, [`BaselineError::NoSchema`] and [`BaselineError::Schema`] for a
    /// missing or newer schema, and [`BaselineError::Invalid`] for a
    /// negative or non-finite value, a noise floor below one, or two entries
    /// of one CPU model.
    pub fn parse(text: &str) -> Result<Self, BaselineError> {
        let table: toml::Table = toml::from_str(text).map_err(BaselineError::Parse)?;
        let schema = table
            .get("schema")
            .and_then(toml::Value::as_integer)
            .ok_or(BaselineError::NoSchema)?;
        let table = migrate(schema, table)?;
        let file: Self = table.try_into().map_err(BaselineError::Parse)?;
        let mut models = std::collections::BTreeSet::new();
        for entry in &file.entries {
            entry.check()?;
            let model = entry.model();
            if !models.insert(model) {
                return Err(BaselineError::Invalid(format!(
                    "two entries for the CPU model {}",
                    model_name(model)
                )));
            }
        }
        Ok(file)
    }

    /// The entry for the CPU model `model`.
    #[must_use]
    pub fn entry(&self, model: Option<&str>) -> Option<&Baseline> {
        self.entries.iter().find(|entry| entry.model() == model)
    }

    /// The models the file holds an entry for, by name.
    #[must_use]
    pub fn models(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| model_name(entry.model()))
            .collect()
    }

    /// The file with `entry` in place of the entry of its CPU model, or
    /// added when it has none; every other entry is kept as it was.
    #[must_use]
    pub fn with(mut self, entry: Baseline) -> Self {
        self.schema = SCHEMA;
        match self
            .entries
            .iter_mut()
            .find(|old| old.model() == entry.model())
        {
            Some(old) => *old = entry,
            None => self.entries.push(entry),
        }
        self.entries.sort_by(|a, b| a.model().cmp(&b.model()));
        self
    }

    /// The file as TOML.
    ///
    /// # Errors
    ///
    /// [`BaselineError::Serialize`] if it does not serialize.
    pub fn to_toml(&self) -> Result<String, BaselineError> {
        let body = toml::to_string(self).map_err(BaselineError::Serialize)?;
        Ok(format!(
            "# The performance baseline of ferrohealth-perf, one [[baseline]] per CPU model.\n\
             # `ferrohealth-perf run --update` writes or lowers the entry of the runner's\n\
             # model; a raise is a hand edit in a change that says why.\n{body}"
        ))
    }

    /// Writes the file to `path`, through a rename.
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

impl Baseline {
    /// The CPU model the entry was measured on, its key.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.host.cpu_model.as_deref()
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
}

/// Brings a baseline file of `schema` to [`SCHEMA`], one schema at a time.
/// Schema 1 held one baseline at the top level, which becomes the one
/// entry, keyed by the CPU model its host names. Schema 2 entries were
/// measured within one run, so schema 3 keeps them marked for
/// re-measurement.
fn migrate(schema: i64, mut table: toml::Table) -> Result<toml::Table, BaselineError> {
    match schema {
        SCHEMA => Ok(table),
        2 => {
            if let Some(toml::Value::Array(entries)) = table.get_mut("baseline") {
                for entry in entries.iter_mut() {
                    if let toml::Value::Table(entry) = entry {
                        entry.insert("remeasure".to_owned(), toml::Value::Boolean(true));
                    }
                }
            }
            table.insert("schema".to_owned(), toml::Value::Integer(SCHEMA));
            Ok(table)
        }
        1 => {
            table.remove("schema");
            let mut file = toml::Table::new();
            file.insert("schema".to_owned(), toml::Value::Integer(2));
            file.insert(
                "baseline".to_owned(),
                toml::Value::Array(vec![toml::Value::Table(table)]),
            );
            migrate(2, file)
        }
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
    /// The entry predated the noise measured across runs, so it was
    /// measured again and written afresh.
    Remeasured,
}

/// The inputs of an update besides the old baseline.
#[derive(Debug, Clone)]
pub struct Update<'a> {
    /// The median of each metric over the update's load phases.
    pub metrics: &'a BTreeMap<Metric, f64>,
    /// The noise floor across those phases ([`across`]).
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

/// The entry an update writes for the run's CPU model, and what it did to
/// each metric; `old` is the entry of that model, if the file holds one.
///
/// With no old entry, one of another plan, or one marked for
/// re-measurement, the update's medians become the entry. On the same plan
/// each metric moves to the better of the entry and the update's median,
/// and a metric new to the update is added. The tolerances of the old entry
/// are kept.
#[must_use]
pub fn updated(old: Option<&Baseline>, update: &Update<'_>) -> (Baseline, Vec<Change>) {
    let tolerance = old.map(|b| b.tolerance).unwrap_or_default();
    let fresh = |changes| {
        (
            Baseline {
                tool: update.tool.to_owned(),
                written: update.written.to_owned(),
                plan: update.plan.clone(),
                host: update.host.clone(),
                tolerance,
                metrics: update.metrics.clone(),
                noise: update.noise.clone(),
                remeasure: false,
            },
            changes,
        )
    };
    let Some(old) = old.filter(|b| &b.plan == update.plan && !b.remeasure) else {
        let changes = if old.is_some_and(|b| b.remeasure && &b.plan == update.plan) {
            vec![Change::Remeasured]
        } else if old.is_some() {
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

/// The median of each metric over the load phases `runs`, and its noise
/// floor across them.
///
/// The floor is how far the worst phase strayed from the median, as a ratio
/// of at least one: above it for a metric where higher is worse, below it
/// for throughput. A metric missing from a phase is taken over the phases
/// that hold it; a floor is left out where the median is zero.
#[must_use]
pub fn across(runs: &[BTreeMap<Metric, f64>]) -> (BTreeMap<Metric, f64>, BTreeMap<Metric, f64>) {
    let mut values: BTreeMap<&Metric, Vec<f64>> = BTreeMap::new();
    for run in runs {
        for (metric, value) in run {
            values.entry(metric).or_default().push(*value);
        }
    }
    let mut medians = BTreeMap::new();
    let mut noise = BTreeMap::new();
    for (metric, mut seen) in values {
        seen.sort_by(f64::total_cmp);
        let Some(median) = median(&seen) else {
            continue;
        };
        medians.insert(metric.clone(), median);
        let worst = if metric.kind().lower_is_worse() {
            seen.first().map(|low| median / low)
        } else {
            seen.last().map(|high| high / median)
        };
        if let Some(ratio) = worst.filter(|r| median > 0.0 && r.is_finite()) {
            noise.insert(metric.clone(), ratio.max(1.0));
        }
    }
    (medians, noise)
}

/// The median of `sorted`, the mean of the middle two for an even count.
#[expect(
    clippy::integer_division,
    reason = "the middle index of a slice is defined by the truncating division"
)]
fn median(sorted: &[f64]) -> Option<f64> {
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted.get(middle).copied()
    } else {
        let low = sorted.get(middle.checked_sub(1)?)?;
        let high = sorted.get(middle)?;
        Some(f64::midpoint(*low, *high))
    }
}

#[cfg(test)]
mod tests;
