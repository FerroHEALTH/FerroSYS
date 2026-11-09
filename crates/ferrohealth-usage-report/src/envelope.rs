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
//!
//! The bounds and the patterns are public, so a reader of the envelope
//! holds them from this one place; feature `receive` adds
//! `crate::received`, which reads a report back under every rule.

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

/// The most members `status` carries.
pub const MAX_STATUS_MEMBERS: usize = 32;

/// The most tokens one `status` array holds.
pub const MAX_TOKENS: usize = 16;

/// The longest key or token, in characters.
pub const MAX_NAME_LEN: usize = 32;

/// The longest `version`, in characters.
pub const MAX_VERSION_LEN: usize = 64;

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
#[cfg_attr(feature = "receive", derive(serde::Deserialize))]
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
#[cfg_attr(feature = "receive", derive(serde::Deserialize))]
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
        (first_is_letter && rest_is_valid && name.len() <= MAX_NAME_LEN)
            .then(|| Self(name.to_owned()))
    }

    /// Returns the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The host's logical CPUs, in the envelope's buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "receive", derive(serde::Deserialize))]
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
#[cfg_attr(feature = "receive", derive(serde::Deserialize))]
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
#[cfg_attr(feature = "receive", derive(serde::Deserialize))]
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
    if is_git_sha(revision) {
        revision.to_owned()
    } else {
        String::from("0000000")
    }
}

/// Whether `text` is a `git_sha` the envelope admits: 7 to 40 lower-case
/// hex digits.
#[must_use]
pub fn is_git_sha(text: &str) -> bool {
    (7..=40).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether `text` is a `status` token: a lower-case letter or digit, then
/// up to 31 lower-case letters, digits, `.`, `_`, `+` or `-`.
///
/// # Examples
///
/// ```
/// use ferrohealth_usage_report::envelope::is_token;
///
/// assert!(is_token("1.2.0+build"));
/// assert!(!is_token("http://snomed.info/sct"));
/// ```
#[must_use]
pub fn is_token(text: &str) -> bool {
    let mut bytes = text.bytes();
    let first = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    first
        && text.len() <= MAX_NAME_LEN
        && bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'+' | b'-')
        })
}

/// Whether `text` is an `instance_id`: a UUID in the hyphenated form, in
/// either case.
#[must_use]
pub fn is_instance_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Whether `text` is a `version`: Semantic Versioning 2.0.0 with a major, minor and
/// patch of at most nine digits each, and at most 64 characters.
///
/// # Examples
///
/// ```
/// use ferrohealth_usage_report::envelope::is_version;
///
/// assert!(is_version("1.4.0-rc.1+build.7"));
/// assert!(!is_version("01.4.0"));
/// assert!(!is_version("1.4"));
/// ```
#[must_use]
pub fn is_version(text: &str) -> bool {
    let (rest, build) = match text.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (text, None),
    };
    let (core, pre) = match rest.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (rest, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    text.len() <= MAX_VERSION_LEN
        && parts.len() == 3
        && parts.iter().all(|part| is_numeric(part) && part.len() <= 9)
        && pre.is_none_or(|pre| pre.split('.').all(is_prerelease_identifier))
        && build.is_none_or(|build| {
            build
                .split('.')
                .all(|id| !id.is_empty() && id.bytes().all(is_identifier_byte))
        })
}

/// Whether `id` is `0` or digits without a leading zero.
fn is_numeric(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && (id == "0" || !id.starts_with('0'))
}

/// Whether `id` is a Semantic Versioning pre-release identifier: a numeric one, or one
/// of letters, digits and `-` holding a non-digit.
fn is_prerelease_identifier(id: &str) -> bool {
    is_numeric(id)
        || (!id.is_empty()
            && id.bytes().all(is_identifier_byte)
            && id.bytes().any(|b| !b.is_ascii_digit()))
}

/// Whether `byte` may stand in a Semantic Versioning identifier.
const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

#[cfg(test)]
mod tests {
    use super::{
        CpuBucket, Key, MemoryBucket, RequestsBucket, is_instance_id, is_token, is_version,
        report_git_sha,
    };

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

    #[test]
    fn a_version_follows_the_envelope_pattern() {
        for good in [
            "0.0.0",
            "1.2.3",
            "999999999.0.1",
            "1.0.0-alpha",
            "1.0.0-0.3.7",
            "1.0.0-x-y.1",
            "1.0.0+20130313144700",
            "1.0.0-beta+exp.sha.5114f85",
        ] {
            assert!(is_version(good), "{good} is admitted");
        }
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1000000000.0.0",
            "1.2.3-",
            "1.2.3-01",
            "1.2.3-a..b",
            "1.2.3+",
            "1.2.3+a..b",
            "v1.2.3",
            "1.2.3-\u{e4}",
        ] {
            assert!(!is_version(bad), "{bad:?} is refused");
        }
        assert!(
            !is_version(&format!("1.0.0-{}", "a".repeat(60))),
            "longer than 64 is refused"
        );
    }

    #[test]
    fn a_token_and_an_instance_id_follow_the_envelope_patterns() {
        for good in ["snomed", "icd10cm", "1.2.0+build", "a"] {
            assert!(is_token(good), "{good} is a token");
        }
        for bad in [
            "",
            "-x",
            ".x",
            "Dr. Jansen",
            "http://snomed.info/sct",
            "UPPER",
        ] {
            assert!(!is_token(bad), "{bad:?} is refused");
        }
        assert!(!is_token(&"a".repeat(33)), "longer than 32 is refused");
        assert!(
            is_instance_id("0f8fad5b-d9cb-469f-a165-70867728950e"),
            "lower case"
        );
        assert!(
            is_instance_id("0F8FAD5B-D9CB-469F-A165-70867728950E"),
            "upper case"
        );
        for bad in [
            "0f8fad5bd9cb469fa16570867728950e",
            "0f8fad5b-d9cb-469f-a165-70867728950",
            "0f8fad5b-d9cb-469f-a165-70867728950g",
            "{0f8fad5b-d9cb-469f-a165-70867728950e}",
        ] {
            assert!(!is_instance_id(bad), "{bad} is refused");
        }
    }
}
