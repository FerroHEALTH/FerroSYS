// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of the `[server]` section: its keys, its defaults and its refusals.

use std::collections::BTreeMap;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::Loader;
use ferrohealth_config::units::{ByteSize, Duration};

use super::{BasePath, ServerSection};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    server: ServerSection,
}

fn load(text: &str) -> Result<ServerSection, ConfigError> {
    Loader::new("FERROSYS")?
        .load_sources::<Config>(Some(text), None, &BTreeMap::new())
        .map(|loaded| loaded.into_parts().0.server)
}

#[test]
fn the_section_defaults_and_reads_its_keys() {
    assert_eq!(
        ServerSection::default(),
        load("").expect("an empty file loads")
    );
    let section = load(
        "[server]\nlisten = \"0.0.0.0:9090\"\nbase_path = \"/ferrosys\"\nmax_concurrent_requests = 64\n",
    )
    .expect("the section loads");
    assert_eq!("0.0.0.0:9090", section.listen.to_string());
    assert_eq!("/ferrosys", section.base_path.as_str());
    assert_eq!(
        Some(64),
        section
            .max_concurrent_requests
            .map(std::num::NonZeroUsize::get)
    );
    assert_eq!(Duration::from_secs(10), section.shutdown_timeout);
}

#[test]
fn a_partial_table_keeps_the_product_defaults_and_the_operator_wins() {
    let product = ServerSection {
        listen: "0.0.0.0:8080".parse().expect("an address"),
        body_limit: ByteSize::from_mib(64),
        ..ServerSection::default()
    };
    let load = |text: &str| {
        Loader::new("FERROSYS")?
            .defaults("server", &product)?
            .load_sources::<Config>(Some(text), None, &BTreeMap::new())
            .map(|loaded| loaded.into_parts().0.server)
    };
    let section = load("[server]\nrequest_timeout = \"5s\"\n").expect("the section loads");
    assert_eq!(
        ServerSection {
            request_timeout: Duration::from_secs(5),
            ..product.clone()
        },
        section,
        "a partial table keeps every product default it does not name"
    );
    let section = load("[server]\nlisten = \"127.0.0.1:9090\"\n").expect("the section loads");
    assert_eq!(
        "127.0.0.1:9090",
        section.listen.to_string(),
        "the operator wins"
    );
    assert_eq!(ByteSize::from_mib(64), section.body_limit);
    assert_eq!(product, load("").expect("an empty file loads"));
}

#[test]
fn a_bad_listen_address_base_path_or_zero_bound_is_refused_at_load() {
    for (text, key) in [
        ("[server]\nlisten = \"not an address\"\n", "server.listen"),
        ("[server]\nbase_path = \"/ferrosys/\"\n", "server.base_path"),
        (
            "[server]\nmax_concurrent_requests = 0\n",
            "server.max_concurrent_requests",
        ),
    ] {
        let error = load(text).expect_err("the value is refused");
        assert!(
            matches!(&error, ConfigError::Invalid { key: Some(found), .. } if found == key),
            "{text}: {error:?}"
        );
    }
}

#[test]
fn the_proxy_rate_and_compression_keys_load() {
    let section = load(
        "[server]\ntrusted_proxies = [\"10.0.0.0/8\", \"::1\"]\nforwarded_header = \"x-forwarded-for\"\ncompression = false\n\n[server.caller_rate]\nrequests_per_second = 5\nburst = 10\n",
    )
    .expect("the section loads");
    assert_eq!(
        vec!["10.0.0.0/8".to_owned(), "::1/128".to_owned()],
        section
            .trusted_proxies
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        crate::client_address::ForwardedHeader::XForwardedFor,
        section.forwarded_header
    );
    assert!(!section.compression);
    assert_eq!(Some(10), section.caller_rate.map(|rate| rate.burst.get()));
    for (text, key) in [
        (
            "[server]\ntrusted_proxies = [\"a proxy\"]\n",
            "server.trusted_proxies",
        ),
        (
            "[server.caller_rate]\nrequests_per_second = 0\nburst = 1\n",
            "server.caller_rate.requests_per_second",
        ),
    ] {
        let error = load(text).expect_err("the value is refused");
        let names_key = |error: &ConfigError| matches!(error, ConfigError::Invalid { key: Some(found), .. } if found.starts_with(key));
        let named = match &error {
            ConfigError::Refusals { refusals } => refusals.iter().any(names_key),
            other => names_key(other),
        };
        assert!(named, "{text}: {error:?}");
    }
}

#[test]
fn the_header_bound_and_the_tls_minimum_load() {
    let tls = "[server.tls]\ncertificate_file = \"a.pem\"\nkey_file = \"a.key\"\n";
    let section = load(&format!(
        "[server]\nheader_read_timeout = \"5s\"\n\n{tls}min_version = \"1.2\"\n"
    ))
    .expect("the section loads");
    assert_eq!(Duration::from_secs(5), section.header_read_timeout);
    assert_eq!(
        Some(ferrohealth_config::tls::TlsVersion::Tls12),
        section.tls.map(|tls| tls.min_version),
        "a deployment may lower the minimum to 1.2"
    );
    assert_eq!(
        Duration::from_secs(30),
        ServerSection::default().header_read_timeout
    );
    let default = load(tls).expect("the section loads");
    assert_eq!(
        Some(ferrohealth_config::tls::TlsVersion::Tls13),
        default.tls.map(|tls| tls.min_version),
        "TLS 1.3 is the default minimum"
    );
    assert!(load("[server]\nheader_read_timeout = \"0s\"\n").is_err());
    assert_eq!(
        Duration::from_mins(2),
        ServerSection::default().idle_timeout,
        "the idle limit outlasts the 90 s a Go client keeps an idle connection"
    );
    assert_eq!(
        Duration::from_secs(95),
        load("[server]\nidle_timeout = \"95s\"\n")
            .expect("the section loads")
            .idle_timeout
    );
    assert!(load("[server]\nidle_timeout = \"0s\"\n").is_err());
    let refused = load(&format!("{tls}min_version = \"1.1\"\n")).expect_err("TLS 1.1 is refused");
    assert!(
        refused.to_string().contains("RFC 8996"),
        "the refusal names the deprecation: {refused}"
    );
    assert_eq!(
        Err(ferrohealth_config::tls::TlsVersionError::Deprecated("1.0")),
        ferrohealth_config::tls::TlsVersion::try_from("1.0".to_owned())
    );
}

#[test]
fn the_client_ca_refusal_reads_in_full_through_the_loader() {
    let error = load(
        "[server.tls]\ncertificate_file = \"a.pem\"\nkey_file = \"a.key\"\nclient_ca_file = \"ca.pem\"\n",
    )
    .expect_err("a client CA under none is refused");
    let rendered = error.to_string();
    for word in ["client_ca_file", "none", "required", "optional"] {
        assert!(rendered.contains(word), "{word} in {rendered}");
    }
}

#[test]
fn every_key_of_0_6_is_an_unknown_key() {
    for key in [
        "request_timeout_ms",
        "header_read_timeout_ms",
        "idle_timeout_ms",
        "body_limit_bytes",
        "drain_delay_ms",
        "shutdown_timeout_ms",
        "retry_after_s",
        "compression_min_bytes",
    ] {
        assert!(
            load(&format!("[server]\n{key} = 1\n")).is_err(),
            "{key} is no key of [server]"
        );
    }
    assert!(
        load("[server.tls]\ncertificate_file = \"a.pem\"\nkey_file = \"a.key\"\nreload_interval_s = 60\n")
            .is_err(),
        "reload_interval_s is no key of [server.tls]"
    );
}

#[test]
fn the_durations_and_sizes_read_with_their_units() {
    let section = load(
        "[server]\nretry_after = \"2min\"\nbody_limit = \"64MiB\"\ncompression_min_size = \"2KiB\"\ndrain_delay = \"5s\"\n\n[server.tls]\ncertificate_file = \"a.pem\"\nkey_file = \"a.key\"\nreload_interval = \"1h\"\n",
    )
    .expect("the section loads");
    assert_eq!(Duration::from_mins(2), section.retry_after);
    assert_eq!(64 * 1024 * 1024, section.body_limit_bytes());
    assert_eq!(ByteSize::from_kib(2), section.compression_min_size);
    assert_eq!(Duration::from_secs(5), section.drain_delay);
    assert_eq!(
        Some(Duration::from_hours(1)),
        section.tls.and_then(|tls| tls.reload_interval)
    );
    let rendered = toml::to_string(&ServerSection::default()).expect("the rendering");
    for line in [
        "request_timeout = \"30s\"",
        "idle_timeout = \"2min\"",
        "body_limit = \"1MiB\"",
        "compression_min_size = \"1KiB\"",
    ] {
        assert!(rendered.contains(line), "{line} in {rendered}");
    }
}

#[test]
fn a_sub_second_retry_after_a_bare_number_or_a_zero_reload_is_refused() {
    let tls = "[server.tls]\ncertificate_file = \"a.pem\"\nkey_file = \"a.key\"\n";
    for (text, word) in [
        ("[server]\nretry_after = \"1500ms\"\n", "RFC 9110"),
        ("[server]\nrequest_timeout = 30\n", "`30s`"),
        ("[server]\nbody_limit = \"1MB\"\n", "`MiB`"),
        (
            &format!("{tls}reload_interval = \"0s\"\n"),
            "reload_interval is zero",
        ),
    ] {
        let error = load(text).expect_err("the value is refused");
        assert!(error.to_string().contains(word), "{word} in {error}");
    }
}

#[cfg(feature = "schema")]
#[test]
fn the_schema_names_the_keys_of_0_7_and_none_of_0_6() {
    let schema = serde_json::to_value(schemars::schema_for!(ServerSection)).expect("the schema");
    let properties = schema["properties"].as_object().expect("the keys");
    for key in [
        "request_timeout",
        "body_limit",
        "retry_after",
        "compression_min_size",
        "tls",
    ] {
        assert!(properties.contains_key(key), "{key} in {schema}");
    }
    assert!(!properties.contains_key("request_timeout_ms"), "{schema}");
    assert_eq!(false, schema["additionalProperties"], "{schema}");
    assert_eq!(
        "30s", schema["properties"]["request_timeout"]["default"],
        "{schema}"
    );
    let rendered = schema.to_string();
    assert!(rendered.contains("reload_interval"), "{rendered}");
    assert!(!rendered.contains("reload_interval_s"), "{rendered}");
}

#[test]
fn a_base_path_is_empty_or_slash_separated_segments() {
    assert!(BasePath::new("").is_ok());
    assert!(BasePath::new("/a/b-c").is_ok());
    for bad in ["a", "/", "/a/", "//a", "/a b", "/a?b", "/a#b", "/ä"] {
        assert!(BasePath::new(bad).is_err(), "{bad:?} is refused");
    }
}
