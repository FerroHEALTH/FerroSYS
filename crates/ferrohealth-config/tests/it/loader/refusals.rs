// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The refusals: every key, value, variable and file the loader refuses,
//! and what each refusal names and never echoes.

use std::collections::BTreeMap;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::loader::Loader;

use super::{Config, SENTINEL, environment, loader, refusals};

#[test]
fn an_unknown_key_in_the_file_is_refused_with_its_key_and_a_suggestion() {
    let error = loader()
        .load_sources::<Config>(
            Some("[server]\nlistn = \"0.0.0.0:1\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("an unknown key is refused");
    let ConfigError::Invalid {
        key,
        variable,
        suggestion,
        ..
    } = &error
    else {
        panic!("an unexpected refusal: {error:?}");
    };
    assert_eq!(Some("server.listn"), key.as_deref());
    assert_eq!(None, variable.as_deref());
    assert_eq!(Some("server.listen"), suggestion.as_deref());
    let shown = error.to_string();
    assert!(
        shown.starts_with("`server.listn` is not valid: unknown field `listn`"),
        "{shown}"
    );
    assert!(
        shown.ends_with("; did you mean `server.listen`?"),
        "{shown}"
    );
}

#[test]
fn an_unknown_key_set_by_an_override_names_the_variable() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER__BODY_LIMIT_BYTE", "1")]),
        )
        .expect_err("an unknown key is refused");
    assert!(
        matches!(
            &error,
            ConfigError::Invalid { key: Some(key), variable: Some(variable), suggestion: Some(suggestion), .. }
                if key == "server.body_limit_byte"
                    && variable == "FERROSYS__SERVER__BODY_LIMIT_BYTE"
                    && suggestion == "server.body_limit_bytes"
        ),
        "{error:?}"
    );
}

#[test]
fn an_unknown_section_is_refused_with_the_closest_section() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SERVR__LISTEN", "x")]),
        )
        .expect_err("an unknown section is refused");
    assert!(
        matches!(
            &error,
            ConfigError::Invalid { key: Some(key), variable: Some(variable), suggestion: Some(suggestion), .. }
                if key == "servr"
                    && variable == "FERROSYS__SERVR__LISTEN"
                    && suggestion == "server"
        ),
        "the refusal names the variable that created the section: {error:?}"
    );
}

#[test]
fn a_value_of_the_wrong_type_is_refused_without_quoting_it() {
    let error = loader()
        .load_sources::<Config>(
            Some("[server]\nbody_limit_bytes = \"Qz7sentinel\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("a string is no number");
    assert!(
        matches!(&error, ConfigError::Invalid { key: Some(key), suggestion: None, .. } if key == "server.body_limit_bytes"),
        "{error:?}"
    );
    assert!(
        !error.to_string().contains(SENTINEL),
        "the refusal quotes no value"
    );
    assert!(
        std::error::Error::source(&error).is_none(),
        "the reader's error, which quotes the line, is not kept"
    );
}

#[test]
fn an_override_that_reads_as_no_value_the_tree_takes_is_refused() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER__BODY_LIMIT_BYTES", "many")]),
        )
        .expect_err("the text is no number");
    assert!(
        matches!(&error, ConfigError::Invalid { variable: Some(variable), .. } if variable == "FERROSYS__SERVER__BODY_LIMIT_BYTES"),
        "{error:?}"
    );
}

#[test]
fn an_unknown_variant_is_refused_with_the_closest_variant() {
    let error = loader()
        .load_sources::<Config>(None, None, &environment(&[("FERROSYS_LOG_FORMAT", "jsn")]))
        .expect_err("jsn is no format");
    assert!(
        matches!(&error, ConfigError::Invalid { key: Some(key), variable: Some(variable), suggestion: Some(suggestion), .. }
            if key == "telemetry.format" && variable == "FERROSYS_LOG_FORMAT" && suggestion == "json"),
        "{error:?}"
    );
}

#[test]
fn an_invalid_nested_value_is_refused_once() {
    #[derive(Debug, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    #[expect(dead_code, reason = "the fields are only deserialized")]
    struct Rate {
        requests_per_second: std::num::NonZeroU32,
        burst: std::num::NonZeroU32,
    }
    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Server {
        caller_rate: Option<Rate>,
        port: u16,
    }
    #[derive(Debug, Default, serde::Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Nested {
        server: Server,
    }
    let error = loader()
        .load_sources::<Nested>(
            Some("[server.caller_rate]\nrequests_per_second = 0\nburst = 1\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("a zero rate is refused");
    assert!(
        matches!(&error, ConfigError::Invalid { key: Some(key), .. } if key == "server.caller_rate.requests_per_second"),
        "exactly the one refusal: {error:?}"
    );
    let error = loader()
        .load_sources::<Nested>(
            Some("[server]\nport = \"x\"\n[server.caller_rate]\nrequests_per_second = 0\nburst = 0\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("three values are refused");
    let ConfigError::Refusals { refusals } = &error else {
        panic!("an unexpected refusal: {error:?}");
    };
    let keys: Vec<_> = refusals
        .iter()
        .map(|refusal| match refusal {
            ConfigError::Invalid { key, .. } => key.clone().unwrap_or_default(),
            other => other.to_string(),
        })
        .collect();
    assert!(
        keys.iter().all(|key| key != "server.caller_rate"),
        "no refusal for the table itself: {keys:?}"
    );
    assert!(keys.contains(&"server.port".to_owned()), "{keys:?}");
}

#[test]
fn a_file_that_is_not_toml_is_refused_with_its_position_and_no_value() {
    let error = loader()
        .load_sources::<Config>(
            Some("[server]\nlisten = \"Qz7sentinel\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("an open string is no TOML");
    assert!(
        matches!(
            &error,
            ConfigError::Syntax {
                position: Some((2, _)),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.to_string().contains(SENTINEL), "{error}");
}

#[test]
fn a_typo_in_the_override_form_is_refused() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS_CONSOLE__SERVER__LISTEN", "0.0.0.0:9"),
                ("FERROSYS__SERVER__LISTN", "0.0.0.0:9"),
            ]),
        )
        .expect_err("an override that names no key is refused");
    assert!(
        matches!(
            &error,
            ConfigError::Invalid { key: Some(key), variable: Some(variable), suggestion: Some(suggestion), .. }
                if key == "server.listn"
                    && variable == "FERROSYS__SERVER__LISTN"
                    && suggestion == "server.listen"
        ),
        "{error:?}"
    );
}

#[test]
fn an_override_name_with_an_empty_segment_is_refused() {
    let error = loader()
        .load_sources::<Config>(None, None, &environment(&[("FERROSYS__SERVER__", "x")]))
        .expect_err("every segment names something");
    assert_eq!(
        "FERROSYS__SERVER__ names no configuration key; use FERROSYS__<KEY> or FERROSYS__<SECTION>__<KEY>",
        error.to_string()
    );
}

#[test]
fn a_refusal_inside_an_array_of_tables_names_the_element_and_its_key() {
    let file = "[[pixm.manager]]\nurl = \"https://a.example\"\nmethod = \"get\"\n\n\
                [[pixm.manager]]\nurl = \"https://b.example\"\nmethod = \"put\"\n";
    let error = loader()
        .load_sources::<Config>(Some(file), None, &BTreeMap::new())
        .expect_err("the method is refused");
    assert_eq!(
        vec![(
            Some("pixm.manager[1].method".to_owned()),
            None,
            Some("get".to_owned())
        )],
        refusals(&error)
    );
    assert!(
        error
            .to_string()
            .starts_with("`pixm.manager[1].method` is not valid"),
        "{error}"
    );

    let file = "[[pixm.manager]]\nurl = \"https://a.example\"\nmethod = \"get\"\n\n\
                [[pixm.manager]]\nurl = \"https://b.example\"\nmthod = \"post\"\n";
    let error = loader()
        .load_sources::<Config>(Some(file), None, &BTreeMap::new())
        .expect_err("the unknown key is refused");
    assert_eq!(
        vec![
            (
                Some("pixm.manager[1].mthod".to_owned()),
                None,
                Some("pixm.manager[1].method".to_owned())
            ),
            (Some("pixm.manager[1]".to_owned()), None, None),
        ],
        refusals(&error),
        "the unknown key with its suggestion, then the field it left missing"
    );
}

#[test]
fn a_refusal_inside_an_array_an_override_set_names_the_variable() {
    let error = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[(
                "FERROSYS__PIXM__MANAGER",
                "[{ url = \"https://a.example\", method = \"get\" }, { url = \"https://b.example\", method = \"put\" }]",
            )]),
        )
        .expect_err("the method is refused");
    assert_eq!(
        vec![(
            Some("pixm.manager[1].method".to_owned()),
            Some("FERROSYS__PIXM__MANAGER".to_owned()),
            Some("get".to_owned())
        )],
        refusals(&error)
    );
}

#[test]
fn a_secret_of_the_wrong_type_is_never_echoed() {
    for (value, sentinel) in [
        ("73194426", "73194426"),
        ("-73194426", "73194426"),
        ("7319.4426", "7319"),
        ("7.3194426e3", "3194426"),
        ("true", "true"),
        ("1979-05-27T07:32:00Z", "1979"),
        ("[73194426, 2]", "73194426"),
        ("{ part = 73194426 }", "73194426"),
    ] {
        let file = format!("[cdr]\nurl = \"https://cdr.example.org/\"\npassword = {value}\n");
        let error = loader()
            .load_sources::<Config>(Some(&file), None, &BTreeMap::new())
            .err();
        let Some(ConfigError::Invalid { key, reason, .. }) = &error else {
            panic!("{value}: a refusal of the value: {error:?}");
        };
        assert_eq!(Some("cdr.password"), key.as_deref(), "{value}");
        assert!(reason.contains("expected"), "{value}: {reason}");
        let shown = format!(
            "{error:?} {}",
            error.as_ref().map(ToString::to_string).unwrap_or_default()
        );
        assert!(!shown.contains(sentinel), "{value}: {shown}");
    }
}

#[test]
fn a_secret_and_its_file_sibling_together_are_refused() {
    let loaded = loader()
        .load_sources::<Config>(
            Some("[cdr]\nurl = \"https://cdr.example.org/\"\npassword = \"Qz7sentinel\"\npassword_file = \"/run/secrets/cdr\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect("the tree loads");
    let cdr = loaded
        .settings()
        .cdr
        .as_ref()
        .expect("the section is present");
    let error = ferrohealth_secret::file::read_secret(
        "cdr.password",
        cdr.password.clone(),
        cdr.password_file.as_deref(),
    )
    .map_err(ConfigError::from)
    .expect_err("both are set");
    assert_eq!(
        "cdr.password and cdr.password_file are both set; set exactly one",
        error.to_string()
    );
}

#[test]
fn a_prefix_outside_the_grammar_is_refused() {
    for prefix in [
        "",
        "ferrosys",
        "FERRO__SYS",
        "_FERROSYS",
        "FERROSYS_",
        "1FERRO",
    ] {
        assert!(
            matches!(Loader::new(prefix), Err(ConfigError::Prefix { .. })),
            "{prefix:?} is refused"
        );
    }
    let loader = Loader::new("FERRO_SYS2").expect("words joined by single underscores");
    assert_eq!("FERRO_SYS2", loader.prefix());
    assert_eq!("FERRO_SYS2_CONFIG", loader.config_variable());
}

#[test]
fn a_named_file_that_cannot_be_read_is_refused() {
    let error = loader()
        .load::<Config>(Some(std::path::Path::new("/nonexistent/ferrosys.toml")))
        .expect_err("a missing file is refused");
    assert!(matches!(error, ConfigError::Read { .. }), "{error:?}");
}

#[test]
fn every_refusal_is_reported_at_once() {
    let error = loader()
        .load_sources::<Config>(
            Some("[server]\nlistn = \"x\"\nbody_limit_bytes = \"many\"\n[telemetry]\nformt = \"json\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("three faults are refused");
    let ConfigError::Refusals { refusals } = &error else {
        panic!("an unexpected refusal: {error:?}");
    };
    let mut keys: Vec<String> = refusals
        .iter()
        .filter_map(|refusal| match refusal {
            ConfigError::Invalid { key, .. } => key.clone(),
            _ => None,
        })
        .collect();
    keys.sort();
    assert_eq!(
        vec!["server.body_limit_bytes", "server.listn", "telemetry.formt"],
        keys
    );
}
