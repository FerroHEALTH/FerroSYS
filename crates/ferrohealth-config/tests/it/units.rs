// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The duration and size grammars: what each reads, what each refuses and
//! names, and how each renders, through the loader as well.

use std::collections::BTreeMap;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::Loader;
use ferrohealth_config::units::{ByteSize, Duration, UnitError};
use serde::{Deserialize, Serialize};

#[test]
fn every_ucum_time_unit_reads_to_its_length() {
    for (text, millis) in [
        ("0s", 0),
        ("500ms", 500),
        ("30s", 30_000),
        ("10min", 600_000),
        ("12h", 43_200_000),
        ("7d", 604_800_000),
        ("0ms", 0),
    ] {
        let duration: Duration = text.parse().expect(text);
        assert_eq!(millis, duration.as_millis(), "{text}");
    }
}

#[test]
fn a_duration_renders_in_the_largest_exact_unit() {
    for (text, shown) in [
        ("3600s", "1h"),
        ("90s", "90s"),
        ("1500ms", "1500ms"),
        ("120min", "2h"),
        ("48h", "2d"),
        ("0ms", "0s"),
        ("60000ms", "1min"),
    ] {
        let duration: Duration = text.parse().expect(text);
        assert_eq!(shown, duration.to_string(), "{text}");
    }
}

#[test]
fn a_bare_number_is_refused_naming_a_unit() {
    assert_eq!(
        Err(UnitError::NoUnit {
            kind: "duration",
            example: "30s",
        }),
        "30".parse::<Duration>()
    );
    let shown = "30".parse::<Duration>().expect_err("bare").to_string();
    assert!(shown.contains("`30s`"), "{shown}");
}

#[test]
fn m_is_refused_as_the_metre() {
    let refused = "5m".parse::<Duration>().expect_err("the metre");
    assert_eq!(UnitError::Metre, refused);
    assert!(refused.to_string().contains("write `min`"), "{refused}");
}

#[test]
fn a_malformed_duration_is_refused() {
    for text in [
        "", "s", "-5s", "+5s", "1.5s", "5 s", " 5s", "5s ", "1h30min", "5S", "5sec", "5MIN", "5µs",
    ] {
        assert!(text.parse::<Duration>().is_err(), "{text:?} was read");
    }
}

#[test]
fn an_unknown_time_unit_names_the_units() {
    let refused = "5sec".parse::<Duration>().expect_err("sec");
    assert!(
        refused
            .to_string()
            .contains("`ms`, `s`, `min`, `h` and `d`"),
        "{refused}"
    );
}

#[test]
fn an_overflowing_duration_is_refused() {
    assert_eq!(
        Err(UnitError::Overflow { kind: "duration" }),
        "18446744073709551615d".parse::<Duration>()
    );
    assert_eq!(
        Err(UnitError::Overflow { kind: "duration" }),
        "99999999999999999999999s".parse::<Duration>()
    );
}

#[test]
fn a_duration_converts_and_reports_whole_seconds() {
    let duration: Duration = "1500ms".parse().expect("1500ms");
    assert_eq!(std::time::Duration::from_millis(1500), duration.get());
    assert_eq!(
        std::time::Duration::from_millis(1500),
        std::time::Duration::from(duration)
    );
    assert!(!duration.is_whole_seconds());
    assert_eq!(1, duration.as_secs());
    assert!(Duration::from_secs(2).is_whole_seconds());
    assert_eq!(Duration::from_mins(2), "120s".parse().expect("120s"));
    assert_eq!(Duration::from_hours(24), Duration::from_days(1));
    assert!(Duration::ZERO.is_zero());
}

#[test]
fn every_iec_binary_unit_reads_to_its_length() {
    for (text, bytes) in [
        ("0B", 0),
        ("512B", 512),
        ("1KiB", 1_024),
        ("64MiB", 64 * 1_048_576),
        ("2GiB", 2 * 1_073_741_824),
        ("1TiB", 1_099_511_627_776),
    ] {
        let size: ByteSize = text.parse().expect(text);
        assert_eq!(bytes, size.bytes(), "{text}");
    }
}

#[test]
fn a_size_renders_in_the_largest_exact_unit() {
    for (text, shown) in [
        ("1024KiB", "1MiB"),
        ("1536KiB", "1536KiB"),
        ("1023B", "1023B"),
        ("0MiB", "0B"),
        ("4096MiB", "4GiB"),
    ] {
        let size: ByteSize = text.parse().expect(text);
        assert_eq!(shown, size.to_string(), "{text}");
    }
}

#[test]
fn a_decimal_unit_is_refused_naming_the_binary_one() {
    for (text, unit, binary) in [
        ("64MB", "MB", "MiB"),
        ("1kB", "kB", "KiB"),
        ("1KB", "KB", "KiB"),
        ("2GB", "GB", "GiB"),
        ("1TB", "TB", "TiB"),
    ] {
        assert_eq!(
            Err(UnitError::Decimal { unit, binary }),
            text.parse::<ByteSize>(),
            "{text}"
        );
    }
}

#[test]
fn a_malformed_or_unknown_size_is_refused() {
    for text in [
        "", "64", "64 MiB", "64mib", "64Mi", "1.5MiB", "-1B", "64MiB ",
    ] {
        assert!(text.parse::<ByteSize>().is_err(), "{text:?} was read");
    }
    assert_eq!(
        Err(UnitError::Overflow { kind: "size" }),
        "16777216TiB".parse::<ByteSize>()
    );
}

#[test]
fn a_size_converts_to_usize() {
    let size = ByteSize::from_mib(10);
    assert_eq!(Ok(10 * 1_048_576), usize::try_from(size));
    assert_eq!(ByteSize::from_kib(1024), ByteSize::from_mib(1));
    assert_eq!(ByteSize::from_gib(1).bytes(), 1 << 30);
    assert!(ByteSize::ZERO.is_zero());
}

/// A section with a duration and a size, as a product writes one.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    timeout: Duration,
    body_limit: ByteSize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            body_limit: ByteSize::from_mib(10),
        }
    }
}

/// The tree around [`Limits`].
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Tree {
    limits: Limits,
}

#[expect(clippy::expect_used, reason = "test fixture over a literal prefix")]
fn load(file: Option<&str>, environment: &[(&str, &str)]) -> Result<Tree, ConfigError> {
    let environment: BTreeMap<String, String> = environment
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    Loader::new("UNITS")
        .expect("the prefix")
        .load_sources::<Tree>(file, None, &environment)
        .map(|loaded| loaded.into_parts().0)
}

#[test]
fn the_file_and_the_environment_write_the_same_strings() {
    let tree = load(
        Some("[limits]\ntimeout = \"2min\"\nbody_limit = \"1MiB\"\n"),
        &[("UNITS__LIMITS__BODY_LIMIT", "512KiB")],
    )
    .expect("the tree");
    assert_eq!(Duration::from_secs(120), tree.limits.timeout);
    assert_eq!(ByteSize::from_kib(512), tree.limits.body_limit);
    let rendered = toml::to_string(&tree).expect("the rendering");
    assert!(rendered.contains("timeout = \"2min\""), "{rendered}");
    assert!(rendered.contains("body_limit = \"512KiB\""), "{rendered}");
}

#[test]
fn the_defaults_hold_when_nothing_is_set() {
    let tree = load(None, &[]).expect("the tree");
    assert_eq!(Duration::from_secs(30), tree.limits.timeout);
    assert_eq!(ByteSize::from_mib(10), tree.limits.body_limit);
}

#[test]
fn a_bare_number_in_the_file_is_refused_with_its_key_and_the_form() {
    let error = load(Some("[limits]\ntimeout = 30\n"), &[]).expect_err("a bare number");
    let shown = error.to_string();
    assert!(shown.contains("limits.timeout"), "{shown}");
    assert!(shown.contains("`30s`"), "{shown}");
}

#[test]
fn a_bare_number_in_the_environment_is_refused_with_its_variable() {
    let error = load(None, &[("UNITS__LIMITS__TIMEOUT", "30")]).expect_err("a bare number");
    let shown = error.to_string();
    assert!(shown.contains("UNITS__LIMITS__TIMEOUT"), "{shown}");
    assert!(shown.contains("`30s`"), "{shown}");
}

#[test]
fn a_decimal_size_in_the_file_is_refused_naming_the_binary_unit() {
    let error = load(Some("[limits]\nbody_limit = \"64MB\"\n"), &[]).expect_err("decimal");
    let shown = error.to_string();
    assert!(shown.contains("limits.body_limit"), "{shown}");
    assert!(shown.contains("`MiB`"), "{shown}");
}

#[cfg(feature = "schema")]
#[test]
fn the_schema_carries_each_grammar() {
    let duration = serde_json::to_value(schemars::schema_for!(Duration)).expect("the schema");
    assert_eq!("^[0-9]+(ms|s|min|h|d)$", duration["pattern"]);
    let size = serde_json::to_value(schemars::schema_for!(ByteSize)).expect("the schema");
    assert_eq!("^[0-9]+(B|KiB|MiB|GiB|TiB)$", size["pattern"]);
    let tls = serde_json::to_value(schemars::schema_for!(
        ferrohealth_config::tls::TlsClientSection
    ))
    .expect("the schema");
    assert!(tls["properties"]["min_version"].is_object(), "{tls}");
}

#[cfg(feature = "schema")]
#[test]
fn the_client_identity_takes_the_secret_schema() {
    let tls = serde_json::to_value(schemars::schema_for!(
        ferrohealth_config::tls::TlsClientSection
    ))
    .expect("the schema");
    assert_eq!(
        serde_json::json!([{ "$ref": "#/$defs/Secret" }, { "type": "null" }]),
        tls["properties"]["client_identity"]["anyOf"],
        "{tls}"
    );
    let secret = &tls["$defs"]["Secret"];
    assert_eq!(serde_json::json!(true), secret["writeOnly"], "{tls}");
    assert_eq!(serde_json::json!("string"), secret["type"], "{tls}");
}
