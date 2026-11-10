// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A product's defaults for a shared section it embeds, shaped as the
//! endpoint table of a shared crate: a URL, a timeout, a nested `retry`
//! table and a credential with its `_file` sibling.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::Loader;
use ferrohealth_config::provenance::Source;
use ferrohealth_config::units::Duration;
use ferrohealth_secret::secret::Secret;
use ferrohealth_secret::secret_url::SecretUrl;

use super::{SENTINEL, environment, refusals};

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Product {
    server_admin: Endpoint,
}

/// The section as the shared crate declares it, with the crate's defaults.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Endpoint {
    url: SecretUrl,
    timeout: Duration,
    retry: Retry,
    credentials: Credentials,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            url: SecretUrl::default(),
            timeout: Duration::from_secs(10),
            retry: Retry::default(),
            credentials: Credentials::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Retry {
    max_attempts: u32,
    initial_backoff: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(200),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Credentials {
    user: String,
    password: Option<Secret>,
    password_file: Option<PathBuf>,
}

/// The product's defaults: a call that waits 30 minutes and is not repeated.
fn product() -> Endpoint {
    Endpoint {
        url: SecretUrl::new("http://127.0.0.1:8081"),
        timeout: Duration::from_mins(30),
        retry: Retry {
            max_attempts: 1,
            ..Retry::default()
        },
        ..Endpoint::default()
    }
}

#[expect(clippy::expect_used, reason = "test fixture over literal defaults")]
fn loader_with(endpoint: &Endpoint) -> Loader {
    Loader::new("FERROSYS")
        .expect("the prefix is valid")
        .defaults("server_admin", endpoint)
        .expect("the defaults serialize")
}

fn assert_product_defaults_kept(endpoint: &Endpoint) {
    assert_eq!(
        Duration::from_mins(30),
        endpoint.timeout,
        "not the crate's 10s"
    );
    assert_eq!(
        1, endpoint.retry.max_attempts,
        "not the crate's three tries"
    );
    assert_eq!(
        Duration::from_millis(200),
        endpoint.retry.initial_backoff,
        "a product default the product left as the crate's stands"
    );
}

#[test]
fn a_partial_endpoint_table_keeps_every_product_default_it_does_not_name() {
    let url = "https://server.example.org:8081";
    let file = format!("[server_admin]\nurl = \"{url}\"\n");
    let cases = [
        (
            Source::File,
            loader_with(&product()).load_sources::<Product>(Some(&file), None, &BTreeMap::new()),
        ),
        (
            Source::Environment,
            loader_with(&product()).load_sources::<Product>(
                None,
                None,
                &environment(&[("FERROSYS__SERVER_ADMIN__URL", url)]),
            ),
        ),
        (
            Source::Set,
            loader_with(&product()).load_sources_with::<Product>(
                None,
                None,
                &BTreeMap::new(),
                &[("server_admin.url".to_owned(), url.to_owned())],
            ),
        ),
    ];
    for (source, loaded) in cases {
        let loaded = loaded.expect("the partial table loads");
        let endpoint = &loaded.settings().server_admin;
        assert_eq!(url, endpoint.url.expose(), "the operator's key wins");
        assert_product_defaults_kept(endpoint);
        let provenance = loaded.provenance();
        assert_eq!(source, provenance.source("server_admin.url"));
        assert_eq!(Source::Default, provenance.source("server_admin.timeout"));
        assert_eq!(
            Source::Default,
            provenance.source("server_admin.retry.max_attempts")
        );
    }
}

#[test]
fn a_partial_nested_table_keeps_its_sibling_product_defaults() {
    let loaded = loader_with(&product())
        .load_sources_with::<Product>(
            Some("[server_admin.retry]\ninitial_backoff = \"1s\"\n"),
            None,
            &environment(&[("FERROSYS__SERVER_ADMIN__TIMEOUT", "45min")]),
            &[],
        )
        .expect("the partial tables load");
    let endpoint = &loaded.settings().server_admin;
    assert_eq!("http://127.0.0.1:8081", endpoint.url.expose());
    assert_eq!(Duration::from_mins(45), endpoint.timeout);
    assert_eq!(1, endpoint.retry.max_attempts, "the product default stands");
    assert_eq!(Duration::from_secs(1), endpoint.retry.initial_backoff);
}

#[test]
fn an_unknown_key_inside_the_merged_table_is_still_refused() {
    let error = loader_with(&product())
        .load_sources::<Product>(
            Some("[server_admin]\nurl = \"https://server.example.org\"\ntimout = \"5s\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("an unknown key is refused");
    assert_eq!(
        vec![(
            Some("server_admin.timout".to_owned()),
            None,
            Some("server_admin.timeout".to_owned())
        )],
        refusals(&error)
    );

    let error = loader_with(&product())
        .load_sources::<Product>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER_ADMIN__RETRY__MAX_ATTEMPT", "2")]),
        )
        .expect_err("an unknown nested key is refused");
    assert_eq!(
        vec![(
            Some("server_admin.retry.max_attempt".to_owned()),
            Some("FERROSYS__SERVER_ADMIN__RETRY__MAX_ATTEMPT".to_owned()),
            Some("server_admin.retry.max_attempts".to_owned())
        )],
        refusals(&error)
    );
}

#[test]
fn a_secret_takes_no_product_default_and_the_crate_default_stands() {
    let mut endpoint = product();
    endpoint.credentials.password = Some(Secret::new(SENTINEL));
    let loaded = loader_with(&endpoint)
        .load_sources::<Product>(None, None, &BTreeMap::new())
        .expect("the defaults load");
    let credentials = &loaded.settings().server_admin.credentials;
    assert!(
        credentials.password.is_none(),
        "the redaction marker never stands as a password"
    );
    assert_product_defaults_kept(&loaded.settings().server_admin);
}

#[test]
fn a_product_default_url_that_serializes_masked_is_refused() {
    for url in [
        "https://sync:password@server.example.org",
        "https://server.example.org/?token=abc",
    ] {
        let mut endpoint = product();
        endpoint.url = SecretUrl::new(url);
        let error = Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .defaults("server_admin", &endpoint)
            .expect_err("a masked default is refused");
        let ConfigError::Assemble { source } = &error else {
            panic!("an assembly refusal: {error:?}");
        };
        let reason = source.to_string();
        assert!(reason.contains("`server_admin.url`"), "{reason}");
        assert!(
            !reason.contains("password") && !reason.contains("abc"),
            "{reason}"
        );
    }
}

#[test]
fn a_secret_set_by_a_source_drops_the_product_default_of_its_file_sibling() {
    let mut endpoint = product();
    endpoint.credentials.password_file = Some(PathBuf::from("/run/secrets/product"));

    let loaded = loader_with(&endpoint)
        .load_sources::<Product>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER_ADMIN__CREDENTIALS__PASSWORD", SENTINEL)]),
        )
        .expect("the secret loads");
    let credentials = &loaded.settings().server_admin.credentials;
    assert_eq!(
        Some(SENTINEL),
        credentials.password.as_ref().map(Secret::expose)
    );
    assert!(
        credentials.password_file.is_none(),
        "one half of the pair, never both"
    );

    let loaded = loader_with(&endpoint)
        .load_sources::<Product>(None, None, &BTreeMap::new())
        .expect("the defaults load");
    assert_eq!(
        Some(PathBuf::from("/run/secrets/product")),
        loaded.settings().server_admin.credentials.password_file,
        "without a source, the product's file stands"
    );
}
