// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The two quantities every configuration tree writes the same way: a
//! [`Duration`] and a [`ByteSize`].
//!
//! Each is one string of a whole number and its unit, with no space and no
//! compound: `"500ms"`, `"30s"`, `"10min"`, `"12h"`, `"7d"`; `"512KiB"`,
//! `"64MiB"`. A key that holds one carries no unit in its name
//! (`request_timeout`, never `request_timeout_ms`), so a file, an
//! environment override and a `--set` argument all write the same text.
//!
//! A bare number is refused, so `30` is never read as seconds or as
//! milliseconds. A value renders in the largest unit that holds it exactly,
//! so `config check` shows `"1h"` for an input of `"3600s"`. A refusal
//! quotes only the unit it read, never the value.

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// A span of time, in whole milliseconds.
///
/// The units are the case-sensitive UCUM codes: `ms` (the prefix `m`, UCUM
/// §27 Table 1, on the second), `s` (§28 Table 2), and `min`, `h` and `d`
/// (§31 Table 5). `m` alone is the metre in UCUM (§28 Table 2) and is refused
/// naming `min`. `"0s"` is a legal value; a key that refuses zero says so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration {
    millis: u64,
}

/// The UCUM codes a [`Duration`] takes, largest first, each with its length
/// in milliseconds.
const TIME_UNITS: [(&str, u64); 5] = [
    ("d", 86_400_000),
    ("h", 3_600_000),
    ("min", 60_000),
    ("s", 1_000),
    ("ms", 1),
];

impl Duration {
    /// No time at all.
    pub const ZERO: Self = Self { millis: 0 };

    /// Returns a duration of `millis` milliseconds.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    /// Returns a duration of `secs` seconds, saturating at the largest
    /// duration.
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self {
            millis: secs.saturating_mul(1_000),
        }
    }

    /// Returns a duration of `minutes` minutes, saturating at the largest
    /// duration.
    #[must_use]
    pub const fn from_mins(minutes: u64) -> Self {
        Self {
            millis: minutes.saturating_mul(60_000),
        }
    }

    /// Returns a duration of `hours` hours, saturating at the largest
    /// duration.
    #[must_use]
    pub const fn from_hours(hours: u64) -> Self {
        Self {
            millis: hours.saturating_mul(3_600_000),
        }
    }

    /// Returns a duration of `days` days of 24 hours, saturating at the
    /// largest duration.
    #[must_use]
    pub const fn from_days(days: u64) -> Self {
        Self {
            millis: days.saturating_mul(86_400_000),
        }
    }

    /// The duration in whole milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.millis
    }

    /// The duration in whole seconds, rounded down.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.millis.div_euclid(1_000)
    }

    /// Whether the duration is a whole number of seconds, as a value a
    /// protocol carries in seconds (such as `Retry-After`, RFC 9110
    /// §10.2.3) must be.
    #[must_use]
    pub const fn is_whole_seconds(self) -> bool {
        self.millis.is_multiple_of(1_000)
    }

    /// Whether the duration is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.millis == 0
    }

    /// The duration as a [`std::time::Duration`].
    #[must_use]
    pub const fn get(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.millis)
    }
}

impl From<Duration> for std::time::Duration {
    fn from(duration: Duration) -> Self {
        duration.get()
    }
}

/// A [`Duration`] or a [`ByteSize`] that could not be read.
///
/// The rendering names the unit it read and the form it wants; it never
/// quotes the number, which a misplaced secret could be.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UnitError {
    /// No unit follows the number.
    #[error("a {kind} takes a whole number and a unit, as `{example}`; a bare number has none")]
    NoUnit {
        /// `duration` or `size`.
        kind: &'static str,
        /// A value of the right form.
        example: &'static str,
    },
    /// No whole number leads the value: it is empty, signed, fractional or
    /// spaced.
    #[error("a {kind} is a whole number followed by its unit with no space, as `{example}`")]
    Malformed {
        /// `duration` or `size`.
        kind: &'static str,
        /// A value of the right form.
        example: &'static str,
    },
    /// `m`, the metre in UCUM.
    #[error("`m` is the metre in UCUM; write `min` for minutes")]
    Metre,
    /// A decimal byte unit, where the binary one is meant.
    #[error(
        "`{unit}` is a decimal unit; sizes take the binary units of IEC 80000-13, as `{binary}`"
    )]
    Decimal {
        /// The decimal unit that was written.
        unit: &'static str,
        /// The binary unit to write.
        binary: &'static str,
    },
    /// A unit that is none of the units the kind takes.
    #[error("a {kind} takes one of the units {units}")]
    Unit {
        /// `duration` or `size`.
        kind: &'static str,
        /// The units the kind takes.
        units: &'static str,
    },
    /// The value does not fit.
    #[error("the {kind} is too large")]
    Overflow {
        /// `duration` or `size`.
        kind: &'static str,
    },
}

/// The units a refusal of a duration lists.
const TIME_UNIT_LIST: &str = "`ms`, `s`, `min`, `h` and `d`";

/// The units a refusal of a size lists.
const BYTE_UNIT_LIST: &str = "`B`, `KiB`, `MiB`, `GiB` and `TiB`";

/// The digits and the unit of `text`, or the refusal of a value with no
/// whole number before its unit.
fn split<'text>(
    text: &'text str,
    kind: &'static str,
    example: &'static str,
) -> Result<(u64, &'text str), UnitError> {
    let unit_at = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(unit_at);
    if digits.is_empty() {
        return Err(UnitError::Malformed { kind, example });
    }
    if unit.is_empty() {
        return Err(UnitError::NoUnit { kind, example });
    }
    if !unit
        .chars()
        .all(|character| character.is_ascii_alphabetic())
    {
        return Err(UnitError::Malformed { kind, example });
    }
    let number = digits
        .parse::<u64>()
        .map_err(|_overflow| UnitError::Overflow { kind })?;
    Ok((number, unit))
}

/// Writes `amount` in the largest of `units` that divides it, or as `0`
/// and `zero` when it is zero.
fn render(
    formatter: &mut fmt::Formatter<'_>,
    amount: u64,
    units: &[(&str, u64)],
    zero: &str,
) -> fmt::Result {
    if amount == 0 {
        return write!(formatter, "0{zero}");
    }
    let (unit, length) = units
        .iter()
        .find(|(_, length)| amount.is_multiple_of(*length))
        .copied()
        .unwrap_or(("", 1));
    write!(formatter, "{}{unit}", amount.div_euclid(length))
}

impl FromStr for Duration {
    type Err = UnitError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const KIND: &str = "duration";
        let (number, unit) = split(text, KIND, "30s")?;
        if unit == "m" {
            return Err(UnitError::Metre);
        }
        let length = TIME_UNITS
            .iter()
            .find(|(code, _)| *code == unit)
            .map(|(_, length)| *length)
            .ok_or(UnitError::Unit {
                kind: KIND,
                units: TIME_UNIT_LIST,
            })?;
        number
            .checked_mul(length)
            .map(Self::from_millis)
            .ok_or(UnitError::Overflow { kind: KIND })
    }
}

impl fmt::Display for Duration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(formatter, self.millis, &TIME_UNITS, "s")
    }
}

/// An amount of data, in bytes.
///
/// The units are `B` and the binary multiples of IEC 80000-13: `KiB`
/// (2^10), `MiB` (2^20), `GiB` (2^30) and `TiB` (2^40). The decimal units
/// `kB`, `MB`, `GB` and `TB` are refused naming the binary one, so `"64MB"`
/// never stands for 64 000 000 bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteSize {
    bytes: u64,
}

/// The units a [`ByteSize`] takes, largest first, each with its length in
/// bytes.
const BYTE_UNITS: [(&str, u64); 5] = [
    ("TiB", 1 << 40),
    ("GiB", 1 << 30),
    ("MiB", 1 << 20),
    ("KiB", 1 << 10),
    ("B", 1),
];

/// The decimal units a size refuses, each with the binary unit to write.
const DECIMAL_UNITS: [(&str, &str); 5] = [
    ("kB", "KiB"),
    ("KB", "KiB"),
    ("MB", "MiB"),
    ("GB", "GiB"),
    ("TB", "TiB"),
];

impl ByteSize {
    /// No data at all.
    pub const ZERO: Self = Self { bytes: 0 };

    /// Returns a size of `bytes` bytes.
    #[must_use]
    pub const fn from_bytes(bytes: u64) -> Self {
        Self { bytes }
    }

    /// Returns a size of `kib` KiB, saturating at the largest size.
    #[must_use]
    pub const fn from_kib(kib: u64) -> Self {
        Self {
            bytes: kib.saturating_mul(1 << 10),
        }
    }

    /// Returns a size of `mib` MiB, saturating at the largest size.
    #[must_use]
    pub const fn from_mib(mib: u64) -> Self {
        Self {
            bytes: mib.saturating_mul(1 << 20),
        }
    }

    /// Returns a size of `gib` GiB, saturating at the largest size.
    #[must_use]
    pub const fn from_gib(gib: u64) -> Self {
        Self {
            bytes: gib.saturating_mul(1 << 30),
        }
    }

    /// The size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }

    /// Whether the size is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.bytes == 0
    }
}

impl TryFrom<ByteSize> for usize {
    type Error = std::num::TryFromIntError;

    fn try_from(size: ByteSize) -> Result<Self, Self::Error> {
        Self::try_from(size.bytes)
    }
}

impl FromStr for ByteSize {
    type Err = UnitError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        const KIND: &str = "size";
        let (number, unit) = split(text, KIND, "64MiB")?;
        if let Some((decimal, binary)) = DECIMAL_UNITS.iter().find(|(code, _)| *code == unit) {
            return Err(UnitError::Decimal {
                unit: decimal,
                binary,
            });
        }
        let length = BYTE_UNITS
            .iter()
            .find(|(code, _)| *code == unit)
            .map(|(_, length)| *length)
            .ok_or(UnitError::Unit {
                kind: KIND,
                units: BYTE_UNIT_LIST,
            })?;
        number
            .checked_mul(length)
            .map(Self::from_bytes)
            .ok_or(UnitError::Overflow { kind: KIND })
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(formatter, self.bytes, &BYTE_UNITS, "B")
    }
}

/// Reads a [`Duration`] or a [`ByteSize`] from a string, refusing a number
/// with the kind's own message.
struct UnitVisitor<T> {
    kind: &'static str,
    example: &'static str,
    target: std::marker::PhantomData<T>,
}

impl<T> Visitor<'_> for UnitVisitor<T>
where
    T: FromStr<Err = UnitError>,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a {} such as `{}`", self.kind, self.example)
    }

    fn visit_str<E>(self, text: &str) -> Result<T, E>
    where
        E: de::Error,
    {
        text.parse().map_err(E::custom)
    }

    fn visit_u64<E>(self, _number: u64) -> Result<T, E>
    where
        E: de::Error,
    {
        Err(E::custom(UnitError::NoUnit {
            kind: self.kind,
            example: self.example,
        }))
    }

    fn visit_i64<E>(self, _number: i64) -> Result<T, E>
    where
        E: de::Error,
    {
        Err(E::custom(UnitError::NoUnit {
            kind: self.kind,
            example: self.example,
        }))
    }

    fn visit_f64<E>(self, _number: f64) -> Result<T, E>
    where
        E: de::Error,
    {
        Err(E::custom(UnitError::Malformed {
            kind: self.kind,
            example: self.example,
        }))
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UnitVisitor {
            kind: "duration",
            example: "30s",
            target: std::marker::PhantomData,
        })
    }
}

impl Serialize for Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UnitVisitor {
            kind: "size",
            example: "64MiB",
            target: std::marker::PhantomData,
        })
    }
}

impl Serialize for ByteSize {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

#[cfg(feature = "schema")]
mod schema {
    use std::borrow::Cow;

    use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

    use super::{ByteSize, Duration};

    impl JsonSchema for Duration {
        fn schema_name() -> Cow<'static, str> {
            Cow::Borrowed("Duration")
        }

        fn schema_id() -> Cow<'static, str> {
            Cow::Borrowed("ferrohealth_config::units::Duration")
        }

        fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
            json_schema!({
                "type": "string",
                "pattern": "^[0-9]+(ms|s|min|h|d)$",
                "description": "A whole number and a UCUM time unit with no space: ms, s, min, h or d, as \"30s\" or \"10min\".",
                "examples": ["500ms", "30s", "10min", "12h", "7d"]
            })
        }
    }

    impl JsonSchema for ByteSize {
        fn schema_name() -> Cow<'static, str> {
            Cow::Borrowed("ByteSize")
        }

        fn schema_id() -> Cow<'static, str> {
            Cow::Borrowed("ferrohealth_config::units::ByteSize")
        }

        fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
            json_schema!({
                "type": "string",
                "pattern": "^[0-9]+(B|KiB|MiB|GiB|TiB)$",
                "description": "A whole number and an IEC 80000-13 binary unit with no space: B, KiB, MiB, GiB or TiB, as \"64MiB\".",
                "examples": ["512KiB", "64MiB", "1GiB"]
            })
        }
    }
}
