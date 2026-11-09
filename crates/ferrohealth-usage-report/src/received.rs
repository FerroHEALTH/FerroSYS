// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A report a collector received, read back under every rule of envelope
//! v2.
//!
//! [`read`] takes the body of one report and returns the closed
//! [`Report`] the reporter writes, or a [`Refusal`] that names the member
//! and the rule it breaks: an unknown or repeated member, a missing one, a
//! value outside its set, bound or pattern, a `first_start` on a `daily`
//! report or `metrics` on a `start` report. The bounds and the patterns
//! are the ones [`crate::envelope`] holds, the schema's
//! (`docs/specs/ferropulse/schemas/report-envelope.v2.json`). The
//! product's `status` is checked against the schema's generic `status`
//! rules and kept as raw JSON in [`Status`], for the product's own reader.
//!
//! ```
//! use ferrohealth_usage_report::received::{Rule, read};
//!
//! let refusal = read(br#"{"schema": 3}"#).unwrap_err();
//! assert!(matches!(refusal.rule(), Rule::Unknown | Rule::Missing | Rule::Const));
//! ```

mod strict;

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::envelope::{
    BUCKETS, Event, GroupStats, Key, MAX_COUNT, MAX_COUNTERS, MAX_GROUPS, MAX_MS,
    MAX_STATUS_MEMBERS, MAX_TOKENS, MAX_UPTIME_S, Metrics, Report, SchemaV2, Window24h, is_git_sha,
    is_instance_id, is_token, is_version,
};

/// The members of a report, `first_start` and `metrics` each on its own
/// event.
const REPORT_MEMBERS: [&str; 14] = [
    "schema",
    "event",
    "first_start",
    "product",
    "instance_id",
    "version",
    "git_sha",
    "licence",
    "deployment",
    "uptime_s",
    "cpu_bucket",
    "memory_bucket",
    "status",
    "metrics",
];

/// The members every report carries.
const REPORT_REQUIRED: [&str; 12] = [
    "schema",
    "event",
    "product",
    "instance_id",
    "version",
    "git_sha",
    "licence",
    "deployment",
    "uptime_s",
    "cpu_bucket",
    "memory_bucket",
    "status",
];

/// The members of `metrics`.
const METRICS_MEMBERS: [&str; 1] = ["window_24h"];

/// The members of `window_24h`.
const WINDOW_MEMBERS: [&str; 3] = ["requests_bucket", "groups", "counters"];

/// The members of one request group.
const GROUP_MEMBERS: [&str; 5] = ["p50_ms", "p95_ms", "p99_ms", "errors_5xx", "histogram"];

/// The product's `status`, checked against the envelope's generic `status`
/// rules and kept as the JSON object it was sent as.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Status(Map<String, Value>);

impl Status {
    /// The members, as sent.
    #[must_use]
    pub const fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// The members, as sent, for the product's own reader.
    #[must_use]
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }
}

/// Why a received report is refused: the member and the rule it breaks.
#[derive(Debug, thiserror::Error)]
#[error("{} {rule}", Member(member))]
pub struct Refusal {
    member: String,
    #[source]
    rule: Rule,
}

impl Refusal {
    /// The member, as a JSON Pointer (RFC 6901); empty for the report
    /// itself.
    #[must_use]
    pub fn member(&self) -> &str {
        &self.member
    }

    /// The rule the member breaks.
    #[must_use]
    pub const fn rule(&self) -> &Rule {
        &self.rule
    }
}

/// A JSON Pointer rendered for a refusal, with the report itself named.
struct Member<'a>(&'a str);

impl fmt::Display for Member<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            formatter.write_str("the report")
        } else {
            formatter.write_str(self.0)
        }
    }
}

/// A rule of envelope v2 a received report breaks.
#[derive(Debug, thiserror::Error)]
pub enum Rule {
    /// The body is no JSON text.
    #[error("is not JSON")]
    Json(#[source] serde_json::Error),
    /// An object names the member twice.
    #[error("is repeated")]
    Repeated,
    /// The envelope declares no such member.
    #[error("is not a member the envelope declares")]
    Unknown,
    /// A required member is absent.
    #[error("is required")]
    Missing,
    /// The member is on the other event: `first_start` on a `daily`
    /// report, `metrics` on a `start` report.
    #[error("does not belong to this event")]
    OtherEvent,
    /// The value has the wrong JSON type.
    #[error("must be {0}")]
    Type(Kind),
    /// `schema` is not `2`.
    #[error("must be 2")]
    Const,
    /// The value is outside the member's set.
    #[error("is not one of the values the envelope admits")]
    NotInSet(#[source] serde_json::Error),
    /// A string breaks the member's pattern.
    #[error("breaks the {0} pattern")]
    Pattern(Pattern),
    /// A number is below its minimum.
    #[error("is below 0")]
    Minimum,
    /// A number is over its maximum.
    #[error("is over its maximum")]
    Maximum,
    /// An object or an array holds more than its bound.
    #[error("holds more than {0}")]
    TooMany(usize),
    /// The histogram holds other than its eleven buckets.
    #[error("must hold exactly {BUCKETS} buckets")]
    Buckets,
    /// A `status` array repeats a token.
    #[error("repeats a token")]
    NotUnique,
    /// A `status` value is none of a boolean, a count, a token or an array
    /// of tokens.
    #[error("must be a boolean, a count, a token or an array of tokens")]
    StatusValue,
}

/// A JSON type a member takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An object.
    Object,
    /// An array.
    Array,
    /// A string.
    String,
    /// `true` or `false`.
    Boolean,
    /// A number with no fraction.
    Integer,
    /// A number.
    Number,
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Object => "an object",
            Self::Array => "an array",
            Self::String => "a string",
            Self::Boolean => "a boolean",
            Self::Integer => "an integer",
            Self::Number => "a number",
        })
    }
}

/// A string pattern of the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// A hyphenated UUID.
    InstanceId,
    /// A bounded Semantic Versioning version.
    Version,
    /// 7 to 40 lower-case hex digits.
    GitSha,
    /// A member name inside `status`, `groups` or `counters`.
    Key,
    /// A `status` token.
    Token,
}

impl fmt::Display for Pattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InstanceId => "instance_id",
            Self::Version => "version",
            Self::GitSha => "git_sha",
            Self::Key => "key",
            Self::Token => "token",
        })
    }
}

/// The event a report names, read before its members.
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum EventKind {
    Start,
    Daily,
}

/// Reads one received report.
///
/// # Errors
///
/// Returns a [`Refusal`] naming the first member found that breaks a rule
/// of envelope v2, and the rule.
pub fn read(body: &[u8]) -> Result<Report<Status>, Refusal> {
    let value = strict::parse(body)?;
    let report = object(&value, "")?;
    members(report, "", &REPORT_MEMBERS, &REPORT_REQUIRED)?;
    let schema = integer(member(report, "schema"), "/schema", u64::MAX)?;
    if schema != 2 {
        return Err(refuse("/schema", Rule::Const));
    }
    let event = match closed::<EventKind>(member(report, "event"), "/event")? {
        EventKind::Start => {
            if report.contains_key("metrics") {
                return Err(refuse("/metrics", Rule::OtherEvent));
            }
            let first_start = report
                .get("first_start")
                .ok_or_else(|| refuse("/first_start", Rule::Missing))?;
            Event::Start {
                first_start: boolean(first_start, "/first_start")?,
            }
        }
        EventKind::Daily => {
            if report.contains_key("first_start") {
                return Err(refuse("/first_start", Rule::OtherEvent));
            }
            let metrics = report
                .get("metrics")
                .ok_or_else(|| refuse("/metrics", Rule::Missing))?;
            Event::Daily {
                metrics: Box::new(read_metrics(metrics)?),
            }
        }
    };
    Ok(Report {
        schema: SchemaV2,
        event,
        product: closed(member(report, "product"), "/product")?,
        instance_id: pattern(
            member(report, "instance_id"),
            "/instance_id",
            is_instance_id,
            Pattern::InstanceId,
        )?,
        version: pattern(
            member(report, "version"),
            "/version",
            is_version,
            Pattern::Version,
        )?,
        git_sha: pattern(
            member(report, "git_sha"),
            "/git_sha",
            is_git_sha,
            Pattern::GitSha,
        )?,
        licence: closed(member(report, "licence"), "/licence")?,
        deployment: closed(member(report, "deployment"), "/deployment")?,
        uptime_s: integer(member(report, "uptime_s"), "/uptime_s", MAX_UPTIME_S)?,
        cpu_bucket: closed(member(report, "cpu_bucket"), "/cpu_bucket")?,
        memory_bucket: closed(member(report, "memory_bucket"), "/memory_bucket")?,
        status: read_status(member(report, "status"))?,
    })
}

/// Reads `metrics`.
fn read_metrics(value: &Value) -> Result<Metrics, Refusal> {
    let metrics = object(value, "/metrics")?;
    members(metrics, "/metrics", &METRICS_MEMBERS, &METRICS_MEMBERS)?;
    let at = "/metrics/window_24h";
    let window = object(member(metrics, "window_24h"), at)?;
    members(window, at, &WINDOW_MEMBERS, &WINDOW_MEMBERS)?;
    let groups_at = format!("{at}/groups");
    let mut groups = BTreeMap::new();
    for (name, group) in keyed(member(window, "groups"), &groups_at, MAX_GROUPS)? {
        let entry_at = pointer(&groups_at, name);
        groups.insert(key(name, &entry_at)?, read_group(group, &entry_at)?);
    }
    let counters_at = format!("{at}/counters");
    let mut counters = BTreeMap::new();
    for (name, count) in keyed(member(window, "counters"), &counters_at, MAX_COUNTERS)? {
        let count_at = pointer(&counters_at, name);
        counters.insert(key(name, &count_at)?, integer(count, &count_at, MAX_COUNT)?);
    }
    Ok(Metrics {
        window_24h: Window24h {
            requests_bucket: closed(
                member(window, "requests_bucket"),
                &format!("{at}/requests_bucket"),
            )?,
            groups,
            counters,
        },
    })
}

/// Reads one request group at `at`.
fn read_group(value: &Value, at: &str) -> Result<GroupStats, Refusal> {
    let group = object(value, at)?;
    members(group, at, &GROUP_MEMBERS, &GROUP_MEMBERS)?;
    let latency = |name: &str| millis(member(group, name), &pointer(at, name));
    let histogram_at = pointer(at, "histogram");
    let Value::Array(buckets) = member(group, "histogram") else {
        return Err(refuse(&histogram_at, Rule::Type(Kind::Array)));
    };
    let counts = buckets
        .iter()
        .enumerate()
        .map(|(index, count)| {
            integer(
                count,
                &pointer(&histogram_at, &index.to_string()),
                MAX_COUNT,
            )
        })
        .collect::<Result<Vec<u64>, Refusal>>()?;
    Ok(GroupStats {
        p50_ms: latency("p50_ms")?,
        p95_ms: latency("p95_ms")?,
        p99_ms: latency("p99_ms")?,
        errors_5xx: integer(
            member(group, "errors_5xx"),
            &pointer(at, "errors_5xx"),
            MAX_COUNT,
        )?,
        histogram: <[u64; BUCKETS]>::try_from(counts)
            .map_err(|_counts| refuse(&histogram_at, Rule::Buckets))?,
    })
}

/// Reads `status` under the envelope's generic rules.
fn read_status(value: &Value) -> Result<Status, Refusal> {
    let at = "/status";
    let status = object(value, at)?;
    if status.len() > MAX_STATUS_MEMBERS {
        return Err(refuse(at, Rule::TooMany(MAX_STATUS_MEMBERS)));
    }
    for (name, fact) in status {
        let fact_at = pointer(at, name);
        key(name, &fact_at)?;
        match fact {
            Value::Bool(_) => {}
            Value::Number(_) => {
                integer(fact, &fact_at, MAX_COUNT)?;
            }
            Value::String(_) => {
                pattern(fact, &fact_at, is_token, Pattern::Token)?;
            }
            Value::Array(tokens) => read_tokens(tokens, &fact_at)?,
            Value::Null | Value::Object(_) => return Err(refuse(&fact_at, Rule::StatusValue)),
        }
    }
    Ok(Status(status.clone()))
}

/// Checks a `status` array: at most [`MAX_TOKENS`] unique tokens.
fn read_tokens(tokens: &[Value], at: &str) -> Result<(), Refusal> {
    if tokens.len() > MAX_TOKENS {
        return Err(refuse(at, Rule::TooMany(MAX_TOKENS)));
    }
    let mut seen = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        let token = pattern(
            token,
            &pointer(at, &index.to_string()),
            is_token,
            Pattern::Token,
        )?;
        if seen.contains(&token) {
            return Err(refuse(at, Rule::NotUnique));
        }
        seen.push(token);
    }
    Ok(())
}

/// Refuses an unknown member of `map`, then a missing required one.
fn members(
    map: &Map<String, Value>,
    at: &str,
    declared: &[&str],
    required: &[&str],
) -> Result<(), Refusal> {
    if let Some(unknown) = map.keys().find(|name| !declared.contains(&name.as_str())) {
        return Err(refuse(&pointer(at, unknown), Rule::Unknown));
    }
    if let Some(missing) = required.iter().find(|name| !map.contains_key(**name)) {
        return Err(refuse(&pointer(at, missing), Rule::Missing));
    }
    Ok(())
}

/// The member `name` of `map`, whose presence [`members`] checked.
fn member<'a>(map: &'a Map<String, Value>, name: &str) -> &'a Value {
    map.get(name).unwrap_or(&Value::Null)
}

/// The value at `at` as an object.
fn object<'a>(value: &'a Value, at: &str) -> Result<&'a Map<String, Value>, Refusal> {
    value
        .as_object()
        .ok_or_else(|| refuse(at, Rule::Type(Kind::Object)))
}

/// The members of the object at `at`, at most `max` of them.
fn keyed<'a>(value: &'a Value, at: &str, max: usize) -> Result<&'a Map<String, Value>, Refusal> {
    let map = object(value, at)?;
    if map.len() > max {
        return Err(refuse(at, Rule::TooMany(max)));
    }
    Ok(map)
}

/// `name` as a [`Key`], the member at `at`.
fn key(name: &str, at: &str) -> Result<Key, Refusal> {
    Key::new(name).ok_or_else(|| refuse(at, Rule::Pattern(Pattern::Key)))
}

/// The value at `at` as one of the closed set `T`.
fn closed<T: DeserializeOwned>(value: &Value, at: &str) -> Result<T, Refusal> {
    T::deserialize(value).map_err(|source| refuse(at, Rule::NotInSet(source)))
}

/// The value at `at` as a boolean.
fn boolean(value: &Value, at: &str) -> Result<bool, Refusal> {
    value
        .as_bool()
        .ok_or_else(|| refuse(at, Rule::Type(Kind::Boolean)))
}

/// The value at `at` as a string that `check` admits.
fn pattern(
    value: &Value,
    at: &str,
    check: fn(&str) -> bool,
    which: Pattern,
) -> Result<String, Refusal> {
    let text = value
        .as_str()
        .ok_or_else(|| refuse(at, Rule::Type(Kind::String)))?;
    if check(text) {
        Ok(text.to_owned())
    } else {
        Err(refuse(at, Rule::Pattern(which)))
    }
}

/// The value at `at` as an integer from 0 to `max`; a number with a zero
/// fraction is an integer, as JSON Schema 2020-12 counts it.
fn integer(value: &Value, at: &str, max: u64) -> Result<u64, Refusal> {
    let Value::Number(number) = value else {
        return Err(refuse(at, Rule::Type(Kind::Integer)));
    };
    let whole = match (number.as_u64(), number.as_f64()) {
        (Some(whole), _) => whole,
        (None, _) if number.is_i64() => return Err(refuse(at, Rule::Minimum)),
        (None, Some(real)) if real.fract().abs() > 0.0 => {
            return Err(refuse(at, Rule::Type(Kind::Integer)));
        }
        (None, Some(real)) if real < 0.0 => return Err(refuse(at, Rule::Minimum)),
        (None, Some(real)) => saturating_whole(real),
        (None, None) => return Err(refuse(at, Rule::Type(Kind::Integer))),
    };
    if whole > max {
        return Err(refuse(at, Rule::Maximum));
    }
    Ok(whole)
}

/// `real`, a non-negative number with no fraction, as an integer, with a
/// value past `u64::MAX` held at `u64::MAX`.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the caller passes a non-negative number with no fraction, and a float cast saturates past u64::MAX, which every bound refuses"
)]
const fn saturating_whole(real: f64) -> u64 {
    real as u64
}

/// The value at `at` as a latency from 0 to [`MAX_MS`].
fn millis(value: &Value, at: &str) -> Result<f64, Refusal> {
    let real = value
        .as_f64()
        .ok_or_else(|| refuse(at, Rule::Type(Kind::Number)))?;
    if real < 0.0 {
        return Err(refuse(at, Rule::Minimum));
    }
    if real > MAX_MS {
        return Err(refuse(at, Rule::Maximum));
    }
    Ok(real)
}

/// The JSON Pointer of the member `name` under `at` (RFC 6901 §3).
fn pointer(at: &str, name: &str) -> String {
    format!("{at}/{}", name.replace('~', "~0").replace('/', "~1"))
}

/// A refusal of the member at `at`.
fn refuse(at: &str, rule: Rule) -> Refusal {
    Refusal {
        member: at.to_owned(),
        rule,
    }
}
