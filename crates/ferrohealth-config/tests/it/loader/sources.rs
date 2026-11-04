// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The sources: the defaults, the file, the environment, the overrides, the
//! fallbacks and the renames, and the provenance each leaves.

use std::collections::BTreeMap;
use std::io::Write as _;

use ferrohealth_config::error::ConfigError;
use ferrohealth_config::provenance::Source;
use ferrohealth_secret::secret::Secret;

use super::{Config, Format, SENTINEL, environment, loader, product_defaults};

#[test]
fn a_partial_table_keeps_the_product_defaults_and_every_source_wins_over_them() {
    let loaded = product_defaults()
        .load_sources::<Config>(Some("[server]\ntls = false\n"), None, &BTreeMap::new())
        .expect("the file loads");
    let server = &loaded.settings().server;
    assert_eq!("0.0.0.0:8080", server.listen, "a product default stands");
    assert_eq!(64 << 20, server.body_limit_bytes);
    assert!(!server.tls, "an explicit file value wins");
    assert_eq!(vec!["https://product.example".to_owned()], server.origins);
    assert_eq!(Source::Default, loaded.provenance().source("server.listen"));
    assert_eq!(Source::File, loaded.provenance().source("server.tls"));

    let loaded = product_defaults()
        .load_sources_with::<Config>(
            Some("[server]\nlisten = \"10.0.0.5:8080\"\n"),
            None,
            &environment(&[("FERROSYS__SERVER__BODY_LIMIT_BYTES", "2048")]),
            &[(
                "server.origins".to_owned(),
                "[\"https://set.example\"]".to_owned(),
            )],
        )
        .expect("the sources load");
    let server = &loaded.settings().server;
    assert_eq!("10.0.0.5:8080", server.listen);
    assert_eq!(2048, server.body_limit_bytes);
    assert_eq!(vec!["https://set.example".to_owned()], server.origins);
    assert!(server.tls, "the product default stands beside them");
}

#[test]
fn without_a_server_table_the_product_defaults_stand_and_a_later_call_wins() {
    let loaded = product_defaults()
        .defaults(
            "server",
            &toml::Table::from_iter([("tls".to_owned(), false.into())]),
        )
        .expect("a table serializes")
        .load_sources::<Config>(None, None, &BTreeMap::new())
        .expect("the defaults load");
    assert_eq!("0.0.0.0:8080", loaded.settings().server.listen);
    assert!(!loaded.settings().server.tls, "the later call wins");
    assert_eq!(
        Format::Auto,
        loaded.settings().telemetry.format,
        "a section without product defaults keeps the tree's"
    );
    assert!(matches!(
        loader().defaults("server.listen", "0.0.0.0:8080"),
        Err(ConfigError::Assemble { .. })
    ));
}

#[test]
fn no_file_and_no_environment_leave_the_defaults() {
    let loaded = loader()
        .load_sources::<Config>(None, None, &BTreeMap::new())
        .expect("the defaults load");
    assert_eq!("127.0.0.1:8080", loaded.settings().server.listen);
    assert!(loaded.settings().cdr.is_none(), "no section, no lane");
    assert_eq!(Source::Default, loaded.provenance().source("server.listen"));
}

#[test]
fn the_environment_overrides_the_file_and_the_alias_overrides_both() {
    let loaded = loader()
        .load_sources::<Config>(
            Some("[server]\nlisten = \"0.0.0.0:1\"\nbody_limit_bytes = 10\n[telemetry]\nfilter = \"warn\"\n"),
            Some(std::path::Path::new("/etc/ferrosys.toml")),
            &environment(&[
                ("FERROSYS__SERVER__LISTEN", "0.0.0.0:2"),
                ("FERROSYS__TELEMETRY__FILTER", "info"),
                ("RUST_LOG", "debug"),
            ]),
        )
        .expect("the tree loads");
    let settings = loaded.settings();
    assert_eq!("0.0.0.0:2", settings.server.listen);
    assert_eq!(10, settings.server.body_limit_bytes);
    assert_eq!("debug", settings.telemetry.filter);
    let provenance = loaded.provenance();
    assert_eq!(Source::Environment, provenance.source("server.listen"));
    assert_eq!(
        Some("FERROSYS__SERVER__LISTEN"),
        provenance.variable("server.listen")
    );
    assert_eq!(Some("RUST_LOG"), provenance.variable("telemetry.filter"));
    assert_eq!(Source::File, provenance.source("server.body_limit_bytes"));
    assert_eq!(Source::Default, provenance.source("server.tls"));
    assert_eq!(
        Some(std::path::Path::new("/etc/ferrosys.toml")),
        provenance.file()
    );
}

#[test]
fn an_override_is_placed_typed_where_the_tree_wants_a_number_or_a_boolean() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS__SERVER__BODY_LIMIT_BYTES", "2048"),
                ("FERROSYS__SERVER__TLS", "true"),
                ("FERROSYS__SERVER__LISTEN", "8080"),
            ]),
        )
        .expect("the overrides load typed where the tree wants it");
    assert_eq!(2048, loaded.settings().server.body_limit_bytes);
    assert!(loaded.settings().server.tls, "the boolean reads typed");
    assert_eq!(
        "8080",
        loaded.settings().server.listen,
        "a string key keeps the text"
    );
}

#[test]
fn the_table_holds_what_the_sources_set_typed_as_the_tree_took_it() {
    let renaming = product_defaults().rename("server.bind", "server.listen", "0.3.0");
    let file = "[server]\nbind = \"10.0.0.5:8080\"\n[cdr]\nurl = \"https://cdr.example.org/\"\n";
    let environment = environment(&[
        ("FERROSYS__SERVER__BODY_LIMIT_BYTES", "2048"),
        ("FERROSYS__SERVER__TLS", "false"),
        (
            "FERROSYS__SERVER__ORIGINS",
            "https://a.example, https://b.example",
        ),
        ("FERROSYS__CDR__PASSWORD", SENTINEL),
    ]);
    let loaded = renaming
        .load_sources::<Config>(Some(file), None, &environment)
        .expect("the sources load");
    let table = loaded.table_with_secrets();
    let server = table
        .get("server")
        .and_then(toml::Value::as_table)
        .expect("the server table is set");
    assert_eq!(
        Some(&toml::Value::Integer(2048)),
        server.get("body_limit_bytes")
    );
    assert_eq!(Some(&toml::Value::Boolean(false)), server.get("tls"));
    assert_eq!(
        Some(&toml::Value::Array(vec![
            "https://a.example".into(),
            "https://b.example".into(),
        ])),
        server.get("origins")
    );
    assert_eq!(
        Some(&toml::Value::String("10.0.0.5:8080".to_owned())),
        server.get("listen"),
        "a renamed key stands under its new name"
    );
    assert!(!server.contains_key("bind"));
    let cdr = table
        .get("cdr")
        .and_then(toml::Value::as_table)
        .expect("the cdr table is set");
    assert_eq!(
        Some(&toml::Value::String(SENTINEL.to_owned())),
        cdr.get("password"),
        "a secret stands as read"
    );
    assert!(
        !table.contains_key("telemetry") && !cdr.contains_key("user"),
        "a key no source set is absent"
    );
    assert!(
        !format!("{loaded:?}").contains(SENTINEL),
        "the rendering of the load shows no secret"
    );

    let text = toml::to_string(table).expect("the table writes");
    let again = product_defaults()
        .load_sources::<Config>(Some(&text), None, &BTreeMap::new())
        .expect("the table reads again as a file");
    assert_eq!(
        toml::Value::try_from(loaded.settings()).expect("the tree serializes"),
        toml::Value::try_from(again.settings()).expect("the tree serializes"),
        "the table read again gives the same tree"
    );
    assert_eq!(table, again.table_with_secrets());
}

#[test]
fn a_nested_sibling_prefix_is_left_alone() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS_CONSOLE__SERVER__LISTEN", "0.0.0.0:9"),
                ("FERROSYS_CONSOLE_CONFIG", "/etc/console.toml"),
                ("FERROSYS_SYNC__BOGUS", "1"),
                ("FERROSYS_INDEX_ROOT", "/srv/index"),
            ]),
        )
        .expect("another binary's variables are not this one's to refuse");
    assert_eq!("127.0.0.1:8080", loaded.settings().server.listen);
    assert_eq!(Source::Default, loaded.provenance().source("server.listen"));
}

#[test]
fn the_config_variable_and_a_reserved_name_pass() {
    let reserving = loader().reserve("FERROSYS_STATE_DIR");
    assert_eq!(["FERROSYS_STATE_DIR".to_owned()], reserving.reserved());
    let loaded = reserving
        .load_sources::<Config>(
            None,
            None,
            &environment(&[
                ("FERROSYS_CONFIG", "/etc/x.toml"),
                ("FERROSYS_STATE_DIR", "/var/lib/x"),
            ]),
        )
        .expect("reserved names pass");
    assert_eq!(Source::Default, loaded.provenance().source("server.listen"));
}

#[test]
fn a_secret_loads_and_its_file_sibling_is_kept_for_the_resolve_step() {
    let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
    file.write_all(b"Qz7sentinel\n")
        .expect("the secret is written");
    let path = file.path().display().to_string();
    let loaded = loader()
        .load_sources::<Config>(
            Some("[cdr]\nurl = \"https://cdr.example.org/\"\nuser = \"ferrosys\"\n"),
            None,
            &environment(&[("FERROSYS__CDR__PASSWORD_FILE", &path)]),
        )
        .expect("the tree loads");
    let cdr = loaded
        .settings()
        .cdr
        .as_ref()
        .expect("the section is present");
    let password: Secret = ferrohealth_secret::file::require_secret(
        "cdr.password",
        cdr.password.clone(),
        cdr.password_file.as_deref(),
    )
    .map_err(ConfigError::from)
    .expect("the file is read");
    assert_eq!(SENTINEL, password.expose());
    assert!(
        !format!("{:?}", loaded.settings()).contains(SENTINEL),
        "the tree shows no secret"
    );
}

#[test]
fn a_secret_and_its_file_sibling_report_one_source() {
    let cdr = "[cdr]\nurl = \"https://cdr.example.org/\"\n";
    let through_file = format!("{cdr}password_file = \"/run/secrets/cdr\"\n");
    let cases = [
        (
            Source::File,
            loader().load_sources::<Config>(Some(&through_file), None, &BTreeMap::new()),
        ),
        (
            Source::Environment,
            loader().load_sources::<Config>(
                Some(cdr),
                None,
                &environment(&[("FERROSYS__CDR__PASSWORD_FILE", "/run/secrets/cdr")]),
            ),
        ),
        (
            Source::Set,
            loader().load_sources_with::<Config>(
                Some(cdr),
                None,
                &BTreeMap::new(),
                &[("cdr.password".to_owned(), SENTINEL.to_owned())],
            ),
        ),
    ];
    for (expected, loaded) in cases {
        let loaded = loaded.expect("the secret loads");
        let provenance = loaded.provenance();
        assert_eq!(expected, provenance.secret_source("cdr.password"));
        assert_eq!(expected, provenance.secret_source("cdr.password_file"));
        assert_eq!(Source::File, provenance.secret_source("cdr.url"));
        assert_eq!(Source::Default, provenance.secret_source("cdr.user"));
    }
}

#[test]
fn a_top_level_key_is_set_through_the_environment() {
    let loaded = loader()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SPEC_PROFILE", "development")]),
        )
        .expect("the tree loads");
    assert_eq!("development", loaded.settings().spec_profile);
    assert_eq!(
        Some("FERROSYS__SPEC_PROFILE"),
        loaded.provenance().variable("spec_profile")
    );
}

#[test]
fn a_set_argument_wins_over_the_environment_and_names_itself() {
    let loaded = loader()
        .load_sources_with::<Config>(
            None,
            None,
            &environment(&[("FERROSYS__SERVER__BODY_LIMIT_BYTES", "10")]),
            &[("server.body_limit_bytes".to_owned(), "20".to_owned())],
        )
        .expect("the tree loads");
    assert_eq!(20, loaded.settings().server.body_limit_bytes);
    assert_eq!(
        Source::Set,
        loaded.provenance().source("server.body_limit_bytes")
    );
    let error = loader()
        .load_sources_with::<Config>(
            None,
            None,
            &BTreeMap::new(),
            &[("server.lsten".to_owned(), "x".to_owned())],
        )
        .expect_err("an unknown key is refused");
    assert!(
        matches!(&error, ConfigError::Invalid { variable: Some(variable), suggestion: Some(suggestion), .. }
            if variable == "--set server.lsten" && suggestion == "server.listen"),
        "{error:?}"
    );
}

#[test]
fn a_list_key_takes_comma_separated_text_or_a_toml_array() {
    for raw in [
        "https://a.example, https://b.example",
        r#"["https://a.example", "https://b.example"]"#,
    ] {
        let loaded = loader()
            .load_sources::<Config>(
                None,
                None,
                &environment(&[("FERROSYS__SERVER__ORIGINS", raw)]),
            )
            .expect("the list loads");
        assert_eq!(
            vec!["https://a.example", "https://b.example"],
            loaded.settings().server.origins,
            "{raw}"
        );
    }
}

#[test]
fn a_fallback_sits_below_the_canonical_override_and_can_transform() {
    let fallback = || {
        loader().fallback(
            "PORT",
            "server.listen",
            Some(|port: &str| format!("0.0.0.0:{port}")),
        )
    };
    let loaded = fallback()
        .load_sources::<Config>(
            Some("[server]\nlisten = \"127.0.0.1:1\"\n"),
            None,
            &environment(&[("PORT", "9090")]),
        )
        .expect("the tree loads");
    assert_eq!(
        "0.0.0.0:9090",
        loaded.settings().server.listen,
        "the fallback wins over the file"
    );
    let loaded = fallback()
        .load_sources::<Config>(
            None,
            None,
            &environment(&[("PORT", "9090"), ("FERROSYS__SERVER__LISTEN", "10.0.0.1:1")]),
        )
        .expect("the tree loads");
    assert_eq!(
        "10.0.0.1:1",
        loaded.settings().server.listen,
        "the canonical override wins over the fallback"
    );
}

#[test]
fn a_composed_fallback_reads_several_variables_in_the_fallback_order() {
    let composed = || {
        loader()
            .fallback_composed(
                ferrohealth_config::libpq::VARIABLES,
                "cdr.url",
                ferrohealth_config::libpq::url,
            )
            .fallback("DATABASE_URL", "cdr.url", None)
    };
    let libpq = [
        ("PGHOST", "db.internal"),
        ("PGUSER", "app"),
        ("PGPASSWORD", SENTINEL),
    ];
    let loaded = composed()
        .load_sources::<Config>(
            Some("[cdr]\nurl = \"postgres://file\"\n"),
            None,
            &environment(&libpq),
        )
        .expect("the tree loads");
    let cdr = loaded.settings().cdr.as_ref().expect("the section");
    assert_eq!(format!("postgres://app:{SENTINEL}@db.internal"), cdr.url);
    assert_eq!(
        Some(ferrohealth_config::libpq::VARIABLES.join("+").as_str()),
        loaded.provenance().variable("cdr.url"),
        "the provenance names the variables"
    );
    let mut both = libpq.to_vec();
    both.push(("DATABASE_URL", "postgres://url"));
    let loaded = composed()
        .load_sources::<Config>(None, None, &environment(&both))
        .expect("the tree loads");
    assert_eq!(
        "postgres://url",
        loaded.settings().cdr.as_ref().expect("the section").url,
        "the later fallback wins"
    );
    both.push(("FERROSYS__CDR__URL", "postgres://canonical"));
    let loaded = composed()
        .load_sources::<Config>(None, None, &environment(&both))
        .expect("the tree loads");
    assert_eq!(
        "postgres://canonical",
        loaded.settings().cdr.as_ref().expect("the section").url,
        "the canonical override wins over every fallback"
    );
}

#[test]
fn a_renamed_key_is_read_under_its_old_name_and_reported() {
    let renaming = || loader().rename("server.bind", "server.listen", "0.3.0");
    let loaded = renaming()
        .load_sources::<Config>(
            Some("[server]\nbind = \"0.0.0.0:1\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect("the old key is read");
    assert_eq!("0.0.0.0:1", loaded.settings().server.listen);
    assert_eq!(
        vec![ferrohealth_config::loader::Deprecation {
            old: "server.bind".to_owned(),
            new: "server.listen".to_owned(),
            refused_in: "0.3.0".to_owned(),
        }],
        loaded.deprecations()
    );
    let report = ferrohealth_config::report::check_report(&loaded).expect("the tree renders");
    assert!(
        report.contains(
            "# deprecated: server.bind is read as server.listen, and is refused from 0.3.0"
        ),
        "{report}"
    );
    let error = renaming()
        .load_sources::<Config>(
            Some("[server]\nbind = \"0.0.0.0:1\"\nlisten = \"0.0.0.0:2\"\n"),
            None,
            &BTreeMap::new(),
        )
        .expect_err("both names are refused");
    assert_eq!(
        "server.bind was renamed to server.listen; set server.listen only",
        error.to_string()
    );
}
