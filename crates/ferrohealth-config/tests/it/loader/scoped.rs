// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A product's defaults for an optional table and for each element of an
//! array of tables: they apply only under what a source writes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::{Loaded, Loader};
use ferrohealth_config::provenance::Source;
use ferrohealth_config::units::Duration;
use ferrohealth_secret::secret::Secret;
use ferrohealth_secret::secret_url::SecretUrl;

use super::{SENTINEL, environment, refusals};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Gateway {
    pdqm: Option<Service>,
    pixm: Option<Pixm>,
}

/// An optional service: the table turns it on.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Service {
    url: String,
    #[serde(default)]
    endpoint: Endpoint,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Pixm {
    manager: Vec<Service>,
}

/// The section as a shared crate declares it, with the crate's defaults.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Endpoint {
    timeout: Duration,
    max_attempts: u32,
    token: Option<Secret>,
    token_file: Option<PathBuf>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_attempts: 3,
            token: None,
            token_file: None,
        }
    }
}

/// The product's defaults: one second and no second try.
fn product() -> Endpoint {
    Endpoint {
        timeout: Duration::from_secs(1),
        max_attempts: 1,
        ..Endpoint::default()
    }
}

#[expect(clippy::expect_used, reason = "test fixture over literal defaults")]
fn loader_with(endpoint: &Endpoint) -> Loader {
    Loader::new("FERROSYS")
        .expect("the prefix is valid")
        .defaults_if_written("pdqm", "endpoint", endpoint)
        .expect("the defaults serialize")
        .defaults_each("pixm.manager", "endpoint", endpoint)
        .expect("the defaults serialize")
}

fn load(text: Option<&str>, pairs: &[(&str, &str)]) -> Result<Loaded<Gateway>, ConfigError> {
    loader_with(&product()).load_sources::<Gateway>(text, None, &environment(pairs))
}

#[test]
fn an_unwritten_optional_table_stays_absent() {
    let loaded = load(None, &[]).expect("nothing loads");
    assert!(loaded.settings().pdqm.is_none(), "the service stays off");
    assert!(loaded.settings().pixm.is_none(), "no manager appears");

    let loaded = load(Some("[pixm]\nmanager = []\n"), &[]).expect("an empty array loads");
    assert!(
        loaded.settings().pdqm.is_none(),
        "another table turns nothing on"
    );
    assert_eq!(
        Some(0),
        loaded
            .settings()
            .pixm
            .as_ref()
            .map(|pixm| pixm.manager.len())
    );
}

#[test]
fn a_partly_written_optional_table_takes_every_product_default_it_does_not_name() {
    let url = "https://pdqm.example";
    let file = format!("[pdqm]\nurl = \"{url}\"\n");
    let cases = [
        (Source::File, load(Some(&file), &[])),
        (
            Source::Environment,
            load(None, &[("FERROSYS__PDQM__URL", url)]),
        ),
        (
            Source::Set,
            loader_with(&product()).load_sources_with::<Gateway>(
                None,
                None,
                &BTreeMap::new(),
                &[("pdqm.url".to_owned(), url.to_owned())],
            ),
        ),
    ];
    for (source, loaded) in cases {
        let loaded = loaded.expect("the partial table loads");
        let service = loaded.settings().pdqm.as_ref().expect("the service is on");
        assert_eq!(url, service.url);
        assert_eq!(Duration::from_secs(1), service.endpoint.timeout, "not 10s");
        assert_eq!(1, service.endpoint.max_attempts, "not three tries");
        assert_eq!(source, loaded.provenance().source("pdqm.url"));
        assert_eq!(
            Source::Default,
            loaded.provenance().source("pdqm.endpoint.timeout")
        );
    }

    let loaded = load(
        Some("[pdqm]\nurl = \"https://pdqm.example\"\n[pdqm.endpoint]\ntimeout = \"2s\"\n"),
        &[],
    )
    .expect("the partial endpoint loads");
    let endpoint = &loaded.settings().pdqm.as_ref().expect("on").endpoint;
    assert_eq!(
        Duration::from_secs(2),
        endpoint.timeout,
        "the operator wins"
    );
    assert_eq!(
        1, endpoint.max_attempts,
        "its sibling keeps the product default"
    );
    assert_eq!(
        Source::Default,
        loaded.provenance().source("pdqm.endpoint.max_attempts")
    );
}

#[test]
fn two_array_elements_written_differently_each_take_the_product_defaults() {
    let file = "[[pixm.manager]]\nurl = \"https://a.example\"\n\
                [pixm.manager.endpoint]\ntimeout = \"5s\"\n\n\
                [[pixm.manager]]\nurl = \"https://b.example\"\n";
    let loaded = load(Some(file), &[]).expect("the managers load");
    let managers = &loaded.settings().pixm.as_ref().expect("written").manager;
    let [first, second] = managers.as_slice() else {
        panic!("two managers: {managers:?}");
    };
    assert_eq!(Duration::from_secs(5), first.endpoint.timeout);
    assert_eq!(1, first.endpoint.max_attempts);
    assert_eq!(Duration::from_secs(1), second.endpoint.timeout);
    assert_eq!(1, second.endpoint.max_attempts);
    let provenance = loaded.provenance();
    assert_eq!(
        Source::File,
        provenance.source("pixm.manager[0].endpoint.timeout")
    );
    assert_eq!(
        Source::Default,
        provenance.source("pixm.manager[0].endpoint.max_attempts")
    );
    assert_eq!(
        Source::Default,
        provenance.source("pixm.manager[1].endpoint.timeout")
    );
    assert!(
        loaded.settings().pdqm.is_none(),
        "the optional table stays off"
    );
}

#[test]
fn an_unknown_key_inside_each_is_still_refused() {
    let error = load(
        Some("[pdqm]\nurl = \"https://pdqm.example\"\n[pdqm.endpoint]\ntimout = \"2s\"\n"),
        &[],
    )
    .expect_err("an unknown key in the optional table is refused");
    assert_eq!(
        vec![(
            Some("pdqm.endpoint.timout".to_owned()),
            None,
            Some("pdqm.endpoint.timeout".to_owned())
        )],
        refusals(&error)
    );

    let error = load(
        Some(
            "[[pixm.manager]]\nurl = \"https://a.example\"\n\n\
             [[pixm.manager]]\nurl = \"https://b.example\"\n\
             [pixm.manager.endpoint]\nmax_attempt = 2\n",
        ),
        &[],
    )
    .expect_err("an unknown key in an element is refused");
    assert_eq!(
        vec![(
            Some("pixm.manager[1].endpoint.max_attempt".to_owned()),
            None,
            Some("pixm.manager[1].endpoint.max_attempts".to_owned())
        )],
        refusals(&error)
    );
}

#[test]
fn a_secret_set_in_an_element_drops_the_product_default_of_its_file_sibling() {
    let mut endpoint = product();
    endpoint.token_file = Some(PathBuf::from("/run/secrets/product"));
    endpoint.token = Some(Secret::new(SENTINEL));
    let file = format!(
        "[[pixm.manager]]\nurl = \"https://a.example\"\n\
         [pixm.manager.endpoint]\ntoken = \"{SENTINEL}\"\n\n\
         [[pixm.manager]]\nurl = \"https://b.example\"\n"
    );
    let loaded = loader_with(&endpoint)
        .load_sources::<Gateway>(Some(&file), None, &BTreeMap::new())
        .expect("the managers load");
    let managers = &loaded.settings().pixm.as_ref().expect("written").manager;
    let [first, second] = managers.as_slice() else {
        panic!("two managers: {managers:?}");
    };
    assert_eq!(
        Some(SENTINEL),
        first.endpoint.token.as_ref().map(Secret::expose)
    );
    assert!(first.endpoint.token_file.is_none(), "one half of the pair");
    assert!(
        second.endpoint.token.is_none(),
        "the redaction marker never stands as a token"
    );
    assert_eq!(
        Some(PathBuf::from("/run/secrets/product")),
        second.endpoint.token_file,
        "without a source, the product's file stands"
    );
}

#[test]
fn a_scoped_default_wins_over_a_tree_default_and_a_later_call_over_an_earlier() {
    let mut wide = product();
    wide.max_attempts = 7;
    let mut later = product();
    later.timeout = Duration::from_secs(4);
    let loaded = Loader::new("FERROSYS")
        .expect("the prefix is valid")
        .defaults("pdqm.endpoint", &wide)
        .and_then(|loader| loader.defaults_if_written("pdqm", "endpoint", &product()))
        .and_then(|loader| loader.defaults_if_written("pdqm", "", &Later { endpoint: later }))
        .expect("the defaults serialize")
        .load_sources::<Gateway>(
            Some("[pdqm]\nurl = \"https://pdqm.example\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect("the table loads");
    let endpoint = &loaded.settings().pdqm.as_ref().expect("on").endpoint;
    assert_eq!(1, endpoint.max_attempts, "the scoped default wins");
    assert_eq!(
        Duration::from_secs(4),
        endpoint.timeout,
        "the later call wins"
    );
}

/// Defaults for the whole optional table, the endpoint inside it.
#[derive(serde::Serialize)]
struct Later {
    endpoint: Endpoint,
}

#[test]
fn a_masked_product_default_names_its_scoped_key() {
    #[derive(serde::Serialize)]
    struct Masked {
        url: SecretUrl,
    }
    let masked = Masked {
        url: SecretUrl::new("https://user:password@pdqm.example"),
    };
    let reasons = [
        Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .defaults_if_written("pdqm", "endpoint", &masked),
        Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .defaults_each("pixm.manager", "endpoint", &masked),
    ]
    .map(|result| match result {
        Err(ConfigError::Assemble { source }) => source.to_string(),
        other => panic!("an assembly refusal: {other:?}"),
    });
    assert!(reasons[0].contains("`pdqm.endpoint.url`"), "{}", reasons[0]);
    assert!(
        reasons[1].contains("`pixm.manager[].endpoint.url`"),
        "{}",
        reasons[1]
    );
    assert!(reasons.iter().all(|reason| !reason.contains("password")));
}
