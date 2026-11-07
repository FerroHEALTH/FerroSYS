// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The plan a product commits, `perf/plan.toml`: the idle limits, the load
//! settings and the request set.
//!
//! Every table and key is optional except the request set, and an unknown
//! key is refused, so a misspelt limit never falls back to a default
//! unnoticed. No specification governs the format: our own design.
//!
//! ```toml
//! [idle]
//! duration_secs = 60
//! max_cpu_percent = 2.0
//! max_transactions_per_second = 5.0
//!
//! [load]
//! rate = 50.0
//! warmup_secs = 10
//! duration_secs = 60
//!
//! [[requests]]
//! group = "metadata"
//! method = "GET"
//! path = "/fhir/metadata"
//! status = 200
//!
//! [[requests]]
//! group = "create"
//! method = "POST"
//! path = "/fhir/Patient"
//! status = 201
//! body = '{"resourceType":"Patient"}'
//! headers = { content-type = "application/fhir+json" }
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use http::{HeaderName, HeaderValue, Method, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The plan file, parsed and checked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The idle phase and its absolute limits.
    #[serde(default)]
    pub idle: IdleSettings,
    /// The load phase.
    #[serde(default)]
    pub load: LoadSettings,
    /// The request set, sent in order and round again for every arrival.
    pub requests: Vec<Request>,
}

/// The idle phase: no request is sent, and the server must do close to
/// nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdleSettings {
    /// How long the phase measures, in seconds.
    pub duration_secs: u64,
    /// The highest CPU time of the server, as a percentage of one core.
    pub max_cpu_percent: f64,
    /// The most committed and rolled-back transactions per second of the
    /// database.
    pub max_transactions_per_second: f64,
    /// How long the tool waits before the phase, so the statistics of
    /// earlier work reach `pg_stat_database` first, in seconds.
    pub settle_secs: u64,
}

impl Default for IdleSettings {
    fn default() -> Self {
        Self {
            duration_secs: 60,
            max_cpu_percent: 2.0,
            max_transactions_per_second: 5.0,
            // NOTE: PostgreSQL 18 source, pgstat.h PGSTAT_IDLE_INTERVAL: an idle backend
            // flushes its pending statistics within 10 s.
            settle_secs: 11,
        }
    }
}

/// The load phase: an open-loop arrival schedule at a fixed rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoadSettings {
    /// The arrivals per second.
    pub rate: f64,
    /// How long the warm-up runs before measuring, in seconds.
    pub warmup_secs: u64,
    /// How long the phase measures after the warm-up, in seconds.
    pub duration_secs: u64,
    /// The most requests outstanding at once.
    pub max_in_flight: u32,
    /// How long one arrival may wait for an in-flight slot before the run
    /// stops as saturated, in milliseconds.
    pub saturation_ms: u64,
    /// How long one request may take before it counts as failed, in
    /// milliseconds.
    pub timeout_ms: u64,
    /// The p99 of the generator's own lag above which the run is refused,
    /// in milliseconds.
    pub max_lag_ms: u64,
    /// How often the server's memory is sampled for its peak, in
    /// milliseconds.
    pub sample_ms: u64,
    /// How long the tool waits after the phase before it reads the
    /// transaction count, in seconds.
    pub settle_secs: u64,
    /// The ratio of p99 to p50 above which every request group at once
    /// marks a stalled runner, refused as unsteady; above one.
    pub stall_ratio: f64,
}

impl Default for LoadSettings {
    fn default() -> Self {
        Self {
            rate: 50.0,
            warmup_secs: 10,
            duration_secs: 60,
            max_in_flight: 256,
            saturation_ms: 5_000,
            timeout_ms: 10_000,
            max_lag_ms: 10,
            sample_ms: 100,
            settle_secs: 11,
            // NOTE: no specification governs this: our own design; a healthy run's p99 was 1.5 to 3 times its p50, a stalled 9V74 runner's 20 to 60 times.
            stall_ratio: 10.0,
        }
    }
}

impl LoadSettings {
    /// The warm-up.
    #[must_use]
    pub fn warmup(&self) -> Duration {
        Duration::from_secs(self.warmup_secs)
    }

    /// The measured window.
    #[must_use]
    pub fn duration(&self) -> Duration {
        Duration::from_secs(self.duration_secs)
    }
}

/// One request of the set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The group its latency is recorded under: lower-case letters, digits,
    /// `-` and `_`.
    pub group: String,
    /// The HTTP method.
    pub method: String,
    /// The path and query below the base URL, starting with `/`.
    pub path: String,
    /// The status the server must answer.
    pub status: u16,
    /// The body sent, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Header fields sent with it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// A plan file that cannot be read or is refused.
#[derive(Debug, Error)]
pub enum PlanError {
    /// The file could not be read.
    #[error("cannot read the plan {path}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The file is not a plan.
    #[error("the plan {path} does not parse")]
    Parse {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: toml::de::Error,
    },
    /// A value the plan holds is refused.
    #[error("the plan is refused: {0}")]
    Invalid(String),
}

impl Plan {
    /// Reads and checks the plan at `path`.
    ///
    /// # Errors
    ///
    /// [`PlanError::Read`] when the file cannot be read, [`PlanError::Parse`]
    /// when it is not a plan, and [`PlanError::Invalid`] for a value
    /// [`Plan::parse`] refuses.
    pub fn read(path: &Path) -> Result<Self, PlanError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| PlanError::Read {
            path: shown.clone(),
            source,
        })?;
        Self::parse(&text).map_err(|error| match error {
            PlanError::Parse { source, .. } => PlanError::Parse {
                path: shown,
                source,
            },
            other => other,
        })
    }

    /// Parses and checks a plan.
    ///
    /// # Errors
    ///
    /// [`PlanError::Parse`] for text that is not a plan, and
    /// [`PlanError::Invalid`] for an empty request set, a bad request, a
    /// rate that is not finite and positive, a zero measured window or
    /// in-flight budget, or a limit that is negative or not finite.
    pub fn parse(text: &str) -> Result<Self, PlanError> {
        let plan: Self = toml::from_str(text).map_err(|source| PlanError::Parse {
            path: String::from("<text>"),
            source,
        })?;
        plan.check()?;
        Ok(plan)
    }

    fn check(&self) -> Result<(), PlanError> {
        let invalid = |text: String| Err(PlanError::Invalid(text));
        if self.requests.is_empty() {
            return invalid("the request set is empty".to_owned());
        }
        let load = &self.load;
        if !(load.rate.is_finite() && load.rate > 0.0) {
            return invalid(format!(
                "load.rate must be finite and positive, not {}",
                load.rate
            ));
        }
        if load.duration_secs == 0 {
            return invalid("load.duration_secs must be at least 1".to_owned());
        }
        if !(load.stall_ratio.is_finite() && load.stall_ratio > 1.0) {
            return invalid(format!(
                "load.stall_ratio must be finite and above 1, not {}",
                load.stall_ratio
            ));
        }
        if load.max_in_flight == 0 || load.sample_ms == 0 || load.timeout_ms == 0 {
            return invalid(
                "load.max_in_flight, load.sample_ms and load.timeout_ms must be at least 1"
                    .to_owned(),
            );
        }
        let idle = &self.idle;
        for (name, limit) in [
            ("idle.max_cpu_percent", idle.max_cpu_percent),
            (
                "idle.max_transactions_per_second",
                idle.max_transactions_per_second,
            ),
        ] {
            if !(limit.is_finite() && limit >= 0.0) {
                return invalid(format!(
                    "{name} must be finite and not negative, not {limit}"
                ));
            }
        }
        if idle.duration_secs == 0 {
            return invalid("idle.duration_secs must be at least 1".to_owned());
        }
        for (index, request) in self.requests.iter().enumerate() {
            request
                .check()
                .map_err(|reason| PlanError::Invalid(format!("request {index}: {reason}")))?;
        }
        Ok(())
    }

    /// The request groups, sorted and without repeats.
    #[must_use]
    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = self.requests.iter().map(|r| r.group.clone()).collect();
        groups.sort();
        groups.dedup();
        groups
    }
}

impl Request {
    fn check(&self) -> Result<(), String> {
        let group_ok = !self.group.is_empty()
            && self
                .group
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !group_ok {
            return Err(format!(
                "group {:?} must be lower-case letters, digits, '-' and '_'",
                self.group
            ));
        }
        self.method()?;
        if !self.path.starts_with('/') {
            return Err(format!("path {:?} must start with '/'", self.path));
        }
        self.expected()?;
        for (name, value) in &self.headers {
            HeaderName::try_from(name.as_str())
                .map_err(|error| format!("header name {name:?}: {error}"))?;
            HeaderValue::try_from(value.as_str())
                .map_err(|error| format!("the value of header {name:?}: {error}"))?;
        }
        Ok(())
    }

    /// The method, parsed.
    ///
    /// # Errors
    ///
    /// The reason, when the method is not an HTTP token.
    pub fn method(&self) -> Result<Method, String> {
        Method::from_bytes(self.method.as_bytes())
            .map_err(|error| format!("method {:?}: {error}", self.method))
    }

    /// The expected status, parsed.
    ///
    /// # Errors
    ///
    /// The reason, when the status is outside 100 to 599.
    pub fn expected(&self) -> Result<StatusCode, String> {
        StatusCode::from_u16(self.status)
            .ok()
            .filter(|status| (100..600).contains(&status.as_u16()))
            .ok_or_else(|| format!("status {} must lie in 100 to 599", self.status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str =
        "[[requests]]\ngroup = \"read\"\nmethod = \"GET\"\npath = \"/x\"\nstatus = 200\n";

    #[test]
    fn a_plan_of_one_request_takes_every_default() {
        let plan = Plan::parse(ONE).unwrap();
        assert_eq!(IdleSettings::default(), plan.idle);
        assert_eq!(LoadSettings::default(), plan.load);
        assert_eq!(vec!["read".to_owned()], plan.groups());
        assert_eq!(Method::GET, plan.requests[0].method().unwrap());
    }

    #[test]
    fn the_defaults_are_the_issue_s_limits() {
        let idle = IdleSettings::default();
        assert_eq!(60, idle.duration_secs);
        assert!(
            (idle.max_cpu_percent - 2.0).abs() < f64::EPSILON,
            "2 % of one core"
        );
        assert!(
            (idle.max_transactions_per_second - 5.0).abs() < f64::EPSILON,
            "5/s"
        );
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let text = format!("[idle]\nmax_cpu = 3.0\n{ONE}");
        assert!(matches!(Plan::parse(&text), Err(PlanError::Parse { .. })));
    }

    #[test]
    fn a_stall_ratio_of_one_or_less_is_refused() {
        for ratio in ["1.0", "0.5", "inf"] {
            let text = format!("[load]\nstall_ratio = {ratio}\n{ONE}");
            assert!(
                matches!(Plan::parse(&text), Err(PlanError::Invalid(reason)) if reason.contains("stall_ratio")),
                "{ratio}"
            );
        }
        let text = format!("[load]\nstall_ratio = 6.0\n{ONE}");
        assert!((Plan::parse(&text).unwrap().load.stall_ratio - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_empty_request_set_is_refused() {
        assert!(matches!(
            Plan::parse("requests = []\n"),
            Err(PlanError::Invalid(reason)) if reason.contains("empty")
        ));
    }

    #[test]
    fn a_bad_request_is_refused_with_its_index() {
        for (bad, word) in [
            ("group = \"Read\"", "group"),
            ("method = \"G T\"", "method"),
            ("path = \"x\"", "path"),
            ("status = 700", "status"),
            ("headers = { \"a b\" = \"c\" }", "header"),
        ] {
            let key = bad.split(' ').next().unwrap();
            let mut lines: Vec<String> = ONE
                .lines()
                .filter(|line| !line.starts_with(&format!("{key} ")))
                .map(str::to_owned)
                .collect();
            lines.push(bad.to_owned());
            let text = lines.join("\n");
            match Plan::parse(&text) {
                Err(PlanError::Invalid(reason)) => {
                    assert!(reason.starts_with("request 0: "), "{reason}");
                    assert!(reason.contains(word), "{reason}");
                }
                other => panic!("{bad} gave {other:?}"),
            }
        }
    }

    #[test]
    fn a_rate_or_limit_out_of_range_is_refused() {
        for table in [
            "[load]\nrate = 0.0\n",
            "[load]\nrate = -1.0\n",
            "[load]\nduration_secs = 0\n",
            "[load]\nmax_in_flight = 0\n",
            "[idle]\nmax_cpu_percent = -1.0\n",
            "[idle]\nduration_secs = 0\n",
        ] {
            let text = format!("{table}{ONE}");
            assert!(
                matches!(Plan::parse(&text), Err(PlanError::Invalid(_))),
                "{table} is refused"
            );
        }
    }

    #[test]
    fn the_module_example_parses() {
        let text = "[idle]\nduration_secs = 60\nmax_cpu_percent = 2.0\n\
                    max_transactions_per_second = 5.0\n[load]\nrate = 50.0\nwarmup_secs = 10\n\
                    duration_secs = 60\n[[requests]]\ngroup = \"metadata\"\nmethod = \"GET\"\n\
                    path = \"/fhir/metadata\"\nstatus = 200\n[[requests]]\ngroup = \"create\"\n\
                    method = \"POST\"\npath = \"/fhir/Patient\"\nstatus = 201\n\
                    body = '{\"resourceType\":\"Patient\"}'\n\
                    headers = { content-type = \"application/fhir+json\" }\n";
        let plan = Plan::parse(text).unwrap();
        assert_eq!(
            vec!["create".to_owned(), "metadata".to_owned()],
            plan.groups()
        );
        assert_eq!(StatusCode::CREATED, plan.requests[1].expected().unwrap());
    }
}
