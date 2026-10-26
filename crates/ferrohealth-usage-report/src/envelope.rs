// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! FerroPULSE's report envelope v2, as closed types.
//!
//! The field set, the value sets and the bounds are the envelope's
//! (`docs/specs/ferropulse/schemas/report-envelope.v2.json`). A member that is
//! not declared here cannot be serialized; a key is a [`Key`] checked against
//! the envelope's key pattern; the `start` and `daily` events carry exactly
//! the members each admits. The product's `status` is its own type, which
//! the product keeps to the envelope's `status` rules.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};

use crate::section::Deployment;

/// The latency buckets of a histogram.
pub const BUCKETS: usize = 11;

/// The upper bounds of the first ten buckets in milliseconds; the last
/// bucket has none.
pub const BUCKET_BOUNDS_MS: [u32; BUCKETS - 1] = [5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

/// The largest count the envelope takes.
pub const MAX_COUNT: u64 = 10_000_000;

/// The largest latency the envelope takes, in milliseconds.
pub const MAX_MS: f64 = 600_000.0;

/// The longest uptime the envelope takes, in seconds: ten years.
pub const MAX_UPTIME_S: u64 = 315_576_000;

/// The most request groups a window carries.
pub const MAX_GROUPS: usize = 32;

/// The most counters a window carries.
pub const MAX_COUNTERS: usize = 64;

/// The envelope's version, always `2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaV2;

impl Serialize for SchemaV2 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(2)
    }
}

/// The reporting product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Product {
    /// FerroEHR.
    #[serde(rename = "ferroehr")]
    Ehr,
    /// FerroTERM.
    #[serde(rename = "ferroterm")]
    Term,
    /// FerroBRIDGE.
    #[serde(rename = "ferrobridge")]
    Bridge,
    /// FerroFED.
    #[serde(rename = "ferrofed")]
    Fed,
    /// FerroPIX.
    #[serde(rename = "ferropix")]
    Pix,
}

/// The licence grant the installation runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LicenceGrant {
    /// The Additional Use Grant: non-commercial production use.
    NonCommercial,
    /// A commercial licence.
    Commercial,
}

/// A member name inside `status`, `groups` or `counters`: a lower-case
/// letter, then up to 31 lower-case letters, digits or underscores.
///
/// # Examples
///
/// ```
/// use ferrohealth_usage_report::envelope::Key;
///
/// assert_eq!(Some("facade_read"), Key::new("facade_read").as_ref().map(Key::as_str));
/// assert!(Key::new("Facade").is_none());
/// assert!(Key::new("/fhir/{id}").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Key(String);

impl Key {
    /// Returns `name` as a key, or `None` when it breaks the envelope's
    /// pattern.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        let mut characters = name.chars();
        let first_is_letter = characters.next().is_some_and(|c| c.is_ascii_lowercase());
        let rest_is_valid =
            characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        (first_is_letter && rest_is_valid && name.len() <= 32).then(|| Self(name.to_owned()))
    }

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The host's logical CPUs, in the envelope's buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CpuBucket {
    /// One or two.
    #[serde(rename = "1-2")]
    UpTo2,
    /// Three or four.
    #[serde(rename = "3-4")]
    UpTo4,
    /// Five to eight.
    #[serde(rename = "5-8")]
    UpTo8,
    /// Nine to sixteen.
    #[serde(rename = "9-16")]
    UpTo16,
    /// Seventeen or more.
    #[serde(rename = "17+")]
    Above16,
}

impl CpuBucket {
    /// The bucket of `cpus`.
    #[must_use]
    pub const fn of(cpus: usize) -> Self {
        match cpus {
            0..=2 => Self::UpTo2,
            3..=4 => Self::UpTo4,
            5..=8 => Self::UpTo8,
            9..=16 => Self::UpTo16,
            _ => Self::Above16,
        }
    }
}

/// The host's memory in GiB, in the envelope's buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MemoryBucket {
    /// Below 4 GiB.
    #[serde(rename = "<4")]
    Below4,
    /// 4 to 8 GiB.
    #[serde(rename = "4-8")]
    Below8,
    /// 8 to 16 GiB.
    #[serde(rename = "8-16")]
    Below16,
    /// 16 to 32 GiB.
    #[serde(rename = "16-32")]
    Below32,
    /// 32 GiB or more.
    #[serde(rename = "32+")]
    AtLeast32,
}

impl MemoryBucket {
    /// The bucket of `bytes`.
    #[must_use]
    pub const fn of(bytes: u64) -> Self {
        match bytes >> 30 {
            0..=3 => Self::Below4,
            4..=7 => Self::Below8,
            8..=15 => Self::Below16,
            16..=31 => Self::Below32,
            _ => Self::AtLeast32,
        }
    }
}

/// The requests of a window, in the envelope's buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RequestsBucket {
    /// None.
    #[serde(rename = "0")]
    None,
    /// 1 to 99.
    #[serde(rename = "1-100")]
    Below100,
    /// 100 to 999.
    #[serde(rename = "100-1k")]
    Below1k,
    /// 1,000 to 9,999.
    #[serde(rename = "1k-10k")]
    Below10k,
    /// 10,000 to 99,999.
    #[serde(rename = "10k-100k")]
    Below100k,
    /// 100,000 or more.
    #[serde(rename = "100k+")]
    AtLeast100k,
}

impl RequestsBucket {
    /// The bucket of `requests`.
    #[must_use]
    pub const fn of(requests: u64) -> Self {
        match requests {
            0 => Self::None,
            1..=99 => Self::Below100,
            100..=999 => Self::Below1k,
            1_000..=9_999 => Self::Below10k,
            10_000..=99_999 => Self::Below100k,
            _ => Self::AtLeast100k,
        }
    }
}

/// The latency and server errors of one request group in the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupStats {
    /// The median latency, in milliseconds.
    pub p50_ms: f64,
    /// The 95th percentile, in milliseconds.
    pub p95_ms: f64,
    /// The 99th percentile, in milliseconds.
    pub p99_ms: f64,
    /// The `5xx` responses.
    pub errors_5xx: u64,
    /// The requests per latency bucket.
    pub histogram: [u64; BUCKETS],
}

/// The window of the last 24 hours.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window24h {
    /// The requests, as a bucket.
    pub requests_bucket: RequestsBucket,
    /// The groups with requests in the window.
    pub groups: BTreeMap<Key, GroupStats>,
    /// The event counts.
    pub counters: BTreeMap<Key, u64>,
}

/// The `daily` report's metrics.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Metrics {
    /// The last 24 hours.
    pub window_24h: Window24h,
}

/// The event a report records, with the members it alone carries.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    /// A process start.
    Start {
        /// Whether this start created the instance id.
        first_start: bool,
    },
    /// The daily report.
    Daily {
        /// The window.
        metrics: Box<Metrics>,
    },
}

/// One report: the envelope and the product's `status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report<S> {
    /// `2`.
    pub schema: SchemaV2,
    /// The event.
    #[serde(flatten)]
    pub event: Event,
    /// The product.
    pub product: Product,
    /// The installation's random id, hyphenated.
    pub instance_id: String,
    /// The product's version.
    pub version: String,
    /// The commit the product was built from.
    pub git_sha: String,
    /// The licence grant.
    pub licence: LicenceGrant,
    /// The deployment kind.
    pub deployment: Deployment,
    /// The seconds since the process started.
    pub uptime_s: u64,
    /// The host's CPUs.
    pub cpu_bucket: CpuBucket,
    /// The host's memory.
    pub memory_bucket: MemoryBucket,
    /// The product's state facts.
    pub status: S,
}

/// Returns `revision` when it is 7 to 40 lower-case hex digits, as the
/// envelope admits, and `0000000` otherwise.
#[must_use]
pub fn report_git_sha(revision: &str) -> String {
    let valid = (7..=40).contains(&revision.len())
        && revision
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if valid {
        revision.to_owned()
    } else {
        String::from("0000000")
    }
}

#[cfg(test)]
mod tests {
    use super::{CpuBucket, Key, MemoryBucket, RequestsBucket, report_git_sha};

    #[test]
    fn buckets_are_half_open() {
        assert_eq!(RequestsBucket::None, RequestsBucket::of(0));
        assert_eq!(RequestsBucket::Below100, RequestsBucket::of(99));
        assert_eq!(RequestsBucket::Below1k, RequestsBucket::of(100));
        assert_eq!(RequestsBucket::AtLeast100k, RequestsBucket::of(100_000));
        assert_eq!(CpuBucket::UpTo2, CpuBucket::of(1));
        assert_eq!(CpuBucket::UpTo16, CpuBucket::of(16));
        assert_eq!(CpuBucket::Above16, CpuBucket::of(17));
        let gib = 1_u64 << 30;
        assert_eq!(MemoryBucket::Below4, MemoryBucket::of(4 * gib - 1));
        assert_eq!(MemoryBucket::Below8, MemoryBucket::of(4 * gib));
        assert_eq!(MemoryBucket::AtLeast32, MemoryBucket::of(32 * gib));
    }

    #[test]
    fn a_key_follows_the_envelope_pattern() {
        assert!(Key::new("a").is_some());
        assert!(Key::new(&"a".repeat(32)).is_some());
        for bad in ["", "1a", "_a", "aB", "a-b", "a b", "ä"] {
            assert!(Key::new(bad).is_none(), "{bad:?} is refused");
        }
        assert!(Key::new(&"a".repeat(33)).is_none());
    }

    #[test]
    fn a_git_sha_outside_the_contract_reports_zeros() {
        assert_eq!("1a2b3c4d5e6f", report_git_sha("1a2b3c4d5e6f"));
        assert_eq!("0000000", report_git_sha("unknown"));
        assert_eq!("0000000", report_git_sha("1A2B3C4"));
        assert_eq!("0000000", report_git_sha("abc"));
    }
}
