// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! An array of tables, such as `[[auth.issuer]]`, set by element index from
//! the environment and from `--set`, and the refusals of an index.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::provenance::Source;
use ferrohealth_config::report::check_report;

use super::{Config, SENTINEL, environment, loader, refusals};

const TWO_ISSUERS: &str = "[[auth.issuer]]\nurl = \"https://a.example\"\naudience = \"a\"\n\n\
                           [[auth.issuer]]\nurl = \"https://b.example\"\naudience = \"b\"\n";

fn urls(config: &Config) -> Vec<&str> {
    config
        .auth
        .as_ref()
        .map(|auth| {
            auth.issuer
                .iter()
                .map(|issuer| issuer.url.as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_array_of_tables_is_set_from_the_environment_alone() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS__AUTH__ISSUER__0__URL", "https://a.example"),
                ("FERROSYS__AUTH__ISSUER__0__AUDIENCE", "a"),
                ("FERROSYS__AUTH__ISSUER__1__URL", "https://b.example"),
                ("FERROSYS__AUTH__ISSUER__1__TIMEOUT_SECS", "30"),
            ]),
        )
        .expect("the issuers load");
    let settings = loaded.settings();
    assert_eq!(
        vec!["https://a.example", "https://b.example"],
        urls(settings)
    );
    let issuers = &settings.auth.as_ref().expect("the auth section").issuer;
    assert_eq!(Some("a"), issuers[0].audience.as_deref());
    assert_eq!(None, issuers[1].audience.as_deref());
    assert_eq!(30, issuers[1].timeout_secs, "a number is placed typed");
    let provenance = loaded.provenance();
    assert_eq!(Source::Environment, provenance.source("auth.issuer[1].url"));
    assert_eq!(
        Some("FERROSYS__AUTH__ISSUER__1__URL"),
        provenance.variable("auth.issuer[1].url")
    );
    assert_eq!(
        Source::Default,
        provenance.source("auth.issuer[0].timeout_secs")
    );
    assert_eq!(Source::Environment, provenance.source("auth.issuer"));

    let table = loaded.table_with_secrets();
    let reread = loader()
        .load_sources::<Config>(
            Some(&toml::to_string(table).expect("the table writes")),
            None,
            &BTreeMap::new(),
        )
        .expect("the table reads again");
    assert_eq!(urls(settings), urls(reread.settings()));
}

#[test]
fn the_elements_are_numbered_as_numbers_and_not_as_text() {
    let names: Vec<(String, String)> = (0..12)
        .map(|index| {
            (
                format!("FERROSYS__AUTH__ISSUER__{index}__URL"),
                format!("https://{index}.example"),
            )
        })
        .collect();
    let loaded = loader()
        .load_sources::<Config>(None, None, &names.into_iter().collect())
        .expect("twelve issuers load");
    let expected: Vec<String> = (0..12)
        .map(|index| format!("https://{index}.example"))
        .collect();
    assert_eq!(expected, urls(loaded.settings()));
}

#[test]
fn the_environment_overrides_a_file_element_key_by_key_and_appends_the_next() {
    let loaded = loader()
        .load_sources::<Config>(
            Some(TWO_ISSUERS),
            None,
            &environment(&[
                ("FERROSYS__AUTH__ISSUER__1__URL", "https://b2.example"),
                ("FERROSYS__AUTH__ISSUER__2__URL", "https://c.example"),
            ]),
        )
        .expect("the sources load");
    let settings = loaded.settings();
    assert_eq!(
        vec![
            "https://a.example",
            "https://b2.example",
            "https://c.example"
        ],
        urls(settings)
    );
    let issuers = &settings.auth.as_ref().expect("the auth section").issuer;
    assert_eq!(
        Some("b"),
        issuers[1].audience.as_deref(),
        "the file's other keys of the element stand"
    );
    let provenance = loaded.provenance();
    assert_eq!(Source::Environment, provenance.source("auth.issuer[1].url"));
    assert_eq!(Source::File, provenance.source("auth.issuer[1].audience"));
    assert_eq!(Source::File, provenance.source("auth.issuer[0].url"));
    assert_eq!(None, provenance.variable("auth.issuer[0].url"));
}

#[test]
fn a_set_argument_wins_over_the_environment_inside_an_element() {
    let loaded = loader()
        .load_sources_with::<Config>(
            Some(TWO_ISSUERS),
            None,
            &environment(&[("FERROSYS__AUTH__ISSUER__0__URL", "https://env.example")]),
            &[
                (
                    "auth.issuer.0.url".to_owned(),
                    "https://set.example".to_owned(),
                ),
                ("auth.issuer[1].audience".to_owned(), "set".to_owned()),
            ],
        )
        .expect("the sources load");
    let issuers = &loaded
        .settings()
        .auth
        .as_ref()
        .expect("the auth section")
        .issuer;
    assert_eq!("https://set.example", issuers[0].url);
    assert_eq!(Some("set"), issuers[1].audience.as_deref());
    let provenance = loaded.provenance();
    assert_eq!(Source::Set, provenance.source("auth.issuer[0].url"));
    assert_eq!(Source::Set, provenance.source("auth.issuer[1].audience"));
    assert_eq!(Source::File, provenance.source("auth.issuer[1].url"));
}

#[test]
fn an_element_is_set_whole_or_patched_over_a_whole_array() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                (
                    "FERROSYS__AUTH__ISSUER",
                    "[{ url = \"https://a.example\" }]",
                ),
                ("FERROSYS__AUTH__ISSUER__0__AUDIENCE", "a"),
                (
                    "FERROSYS__AUTH__ISSUER__1",
                    "{ url = \"https://b.example\" }",
                ),
            ]),
        )
        .expect("the sources load");
    assert_eq!(
        vec!["https://a.example", "https://b.example"],
        urls(loaded.settings())
    );
    let issuers = &loaded
        .settings()
        .auth
        .as_ref()
        .expect("the auth section")
        .issuer;
    assert_eq!(Some("a"), issuers[0].audience.as_deref());
}

#[test]
fn a_secret_and_its_file_sibling_are_set_inside_an_element() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS__AUTH__ISSUER__0__URL", "https://a.example"),
                (
                    "FERROSYS__AUTH__ISSUER__0__CLIENT_SECRET_FILE",
                    "/run/secrets/a",
                ),
                ("FERROSYS__AUTH__ISSUER__1__URL", "https://b.example"),
                ("FERROSYS__AUTH__ISSUER__1__CLIENT_SECRET", SENTINEL),
            ]),
        )
        .expect("the sources load");
    let issuers = &loaded
        .settings()
        .auth
        .as_ref()
        .expect("the auth section")
        .issuer;
    assert_eq!(
        Some(PathBuf::from("/run/secrets/a")),
        issuers[0].client_secret_file
    );
    assert_eq!(
        Some(SENTINEL),
        issuers[1]
            .client_secret
            .as_ref()
            .map(ferrohealth_secret::secret::Secret::expose)
    );
    let provenance = loaded.provenance();
    assert_eq!(
        Source::Environment,
        provenance.secret_source("auth.issuer[0].client_secret")
    );
    let report = check_report(&loaded).expect("the report renders");
    assert!(!report.contains(SENTINEL), "{report}");
    assert!(
        report.contains("# environment: FERROSYS__AUTH__ISSUER__"),
        "{report}"
    );
}

#[test]
fn an_index_that_skips_an_element_is_refused() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__AUTH__ISSUER__1__URL", "https://b.example")]),
        )
        .expect_err("element 0 is not set");
    assert!(
        matches!(
            &error,
            ConfigError::OverrideGap { name, key, missing: 0 }
                if name == "FERROSYS__AUTH__ISSUER__1__URL" && key == "auth.issuer"
        ),
        "{error:?}"
    );
    assert_eq!(
        "FERROSYS__AUTH__ISSUER__1__URL skips element 0 of `auth.issuer`; number the elements of an array of tables from 0 without a gap",
        error.to_string()
    );

    let error = loader()
        .load_sources::<Config>(
            Some(TWO_ISSUERS),
            None,
            &environment(&[("FERROSYS__AUTH__ISSUER__3__URL", "https://d.example")]),
        )
        .expect_err("element 2 is not set");
    assert!(
        matches!(&error, ConfigError::OverrideGap { missing: 2, .. }),
        "{error:?}"
    );

    let error = loader()
        .load_sources_with::<Config>(
            None,
            None,
            &BTreeMap::new(),
            &[(
                "auth.issuer[1].url".to_owned(),
                "https://b.example".to_owned(),
            )],
        )
        .expect_err("element 0 is not set");
    assert!(
        matches!(&error, ConfigError::OverrideGap { name, missing: 0, .. } if name == "--set auth.issuer[1].url"),
        "{error:?}"
    );
}

#[test]
fn an_index_that_is_no_plain_number_after_a_key_is_refused() {
    for name in [
        "FERROSYS__AUTH__ISSUER__01__URL",
        "FERROSYS__AUTH__ISSUER__99999999999999999999999__URL",
        "FERROSYS__0__URL",
        "FERROSYS__AUTH__ISSUER__0__1__URL",
    ] {
        let error = loader()
            .load_sources::<Config>(None, None, &environment(&[(name, "x")]))
            .expect_err("the index is ambiguous");
        assert!(
            matches!(&error, ConfigError::OverrideIndex { name: refused, .. } if refused == name),
            "{name}: {error:?}"
        );
    }
    let error = loader()
        .load_sources_with::<Config>(
            None,
            None,
            &BTreeMap::new(),
            &[("auth.issuer.00.url".to_owned(), "x".to_owned())],
        )
        .expect_err("the index is ambiguous");
    assert!(
        matches!(&error, ConfigError::OverrideIndex { segment, .. } if segment == "00"),
        "{error:?}"
    );
}

#[test]
fn an_index_under_a_table_or_a_value_is_refused() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER__0__LISTEN", "0.0.0.0:1")]),
        )
        .expect_err("the tree refuses an array where it wants a table");
    assert_eq!(
        vec![(
            Some("server[0]".to_owned()),
            Some("FERROSYS__SERVER__0__LISTEN".to_owned()),
            None
        )],
        refusals(&error)
    );

    let error = loader()
        .load_sources::<Config>(
            Some("[server]\nlisten = \"127.0.0.1:1\"\n"),
            None,
            &environment(&[("FERROSYS__SERVER__0__LISTEN", "0.0.0.0:1")]),
        )
        .expect_err("the file makes server a table");
    assert!(
        matches!(
            &error,
            ConfigError::OverrideArray { name, key }
                if name == "FERROSYS__SERVER__0__LISTEN" && key == "server"
        ),
        "{error:?}"
    );

    let error = loader()
        .load_sources::<Config>(
            Some("[server]\norigins = [\"https://a.example\"]\n"),
            None,
            &environment(&[("FERROSYS__SERVER__ORIGINS__0__HOST", "x")]),
        )
        .expect_err("an element of origins is no table");
    assert!(
        matches!(&error, ConfigError::OverrideArray { key, .. } if key == "server.origins[0]"),
        "{error:?}"
    );
}

#[test]
fn an_unknown_key_inside_an_element_is_refused_naming_the_variable() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS__AUTH__ISSUER__0__URL", "https://a.example"),
                ("FERROSYS__AUTH__ISSUER__0__AUDIENC", "a"),
            ]),
        )
        .expect_err("the unknown key is refused");
    assert_eq!(
        vec![(
            Some("auth.issuer[0].audienc".to_owned()),
            Some("FERROSYS__AUTH__ISSUER__0__AUDIENC".to_owned()),
            Some("auth.issuer[0].audience".to_owned())
        )],
        refusals(&error)
    );

    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__AUTH__ISSUER__0__AUDIENCE", "a")]),
        )
        .expect_err("the element misses its url");
    assert_eq!(
        vec![(
            Some("auth.issuer[0]".to_owned()),
            Some("FERROSYS__AUTH__ISSUER__0__AUDIENCE".to_owned()),
            None
        )],
        refusals(&error)
    );
}
