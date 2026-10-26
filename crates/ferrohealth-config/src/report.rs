// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `config check` rendering of a loaded tree.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::error::ConfigError;
use crate::loader::Loaded;
use crate::provenance::{Source, leaves};

/// Renders the running configuration: every key the tree serializes, one
/// per line as `key = value  # source`, sorted by key, under a line naming
/// the file the load read.
///
/// The tree serializes its secrets through the redacting types of
/// `ferrohealth-secret`, so the rendering shows `***` in place of each one
/// and a URL with its credentials masked. A key whose `source` is the
/// environment also names the variable that set it.
///
/// # Errors
///
/// Returns [`ConfigError::Render`] when the tree does not serialize as a
/// TOML table.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use ferrohealth_config::loader::Loader;
/// use ferrohealth_config::report::check_report;
/// use ferrohealth_secret::secret::Secret;
///
/// #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
/// #[serde(default, deny_unknown_fields)]
/// struct Config {
///     user: String,
///     password: Option<Secret>,
/// }
///
/// let loaded = Loader::new("FERROSYS")?.load_sources::<Config>(
///     Some("user = \"ferrosys\"\npassword = \"Qz7sentinel\"\n"),
///     None,
///     &BTreeMap::new(),
/// )?;
/// let report = check_report(&loaded)?;
/// assert!(report.contains("password = \"***\"  # file"));
/// assert!(!report.contains("Qz7sentinel"));
/// # Ok::<(), ferrohealth_config::error::ConfigError>(())
/// ```
pub fn check_report<T: Serialize>(loaded: &Loaded<T>) -> Result<String, ConfigError> {
    let table = toml::Table::try_from(loaded.settings())
        .map_err(|source| ConfigError::Render { source })?;
    let provenance = loaded.provenance();
    let mut lines = vec![match provenance.file() {
        Some(path) => format!("# file: {}", path.display()),
        None => "# file: none".to_owned(),
    }];
    let mut keys = BTreeSet::new();
    leaves("", &table, &mut keys);
    for key in keys {
        let Some(value) = lookup(&table, &key) else {
            continue;
        };
        let source = provenance.source(&key);
        lines.push(match (source, provenance.variable(&key)) {
            (Source::Environment, Some(variable)) => {
                format!("{key} = {value}  # environment: {variable}")
            }
            _ => format!("{key} = {value}  # {source}"),
        });
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// Returns the value the dotted key `key` addresses in `table`.
fn lookup<'a>(table: &'a toml::Table, key: &str) -> Option<&'a toml::Value> {
    let mut segments = key.split('.');
    let mut value = table.get(segments.next()?)?;
    for segment in segments {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ferrohealth_secret::secret::Secret;
    use ferrohealth_secret::secret_url::SecretUrl;

    use super::check_report;
    use crate::loader::Loader;

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        server: Server,
        db: Db,
    }

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Server {
        listen: String,
        origins: Vec<String>,
    }

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Db {
        url: SecretUrl,
        password: Option<Secret>,
    }

    #[test]
    fn the_report_names_each_key_its_value_and_its_source_and_shows_no_secret() {
        let loaded = Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .load_sources::<Config>(
                Some("[server]\nlisten = \"0.0.0.0:8080\"\n[db]\nurl = \"postgres://u:Qz7sentinel@db.example.org/x\"\n"),
                Some(std::path::Path::new("/etc/ferrosys.toml")),
                &BTreeMap::from([("FERROSYS__DB__PASSWORD".to_owned(), "Qz7sentinel".to_owned())]),
            )
            .expect("the tree loads");
        let report = check_report(&loaded).expect("the tree renders");
        assert_eq!(
            "# file: /etc/ferrosys.toml\n\
             db.password = \"***\"  # environment: FERROSYS__DB__PASSWORD\n\
             db.url = \"postgres://***@db.example.org/x\"  # file\n\
             server.listen = \"0.0.0.0:8080\"  # file\n\
             server.origins = []  # default\n",
            report
        );
        assert!(
            !report.contains("Qz7sentinel"),
            "the report shows no secret"
        );
    }

    #[test]
    fn a_load_without_a_file_says_so() {
        let loaded = Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .load_sources::<Config>(None, None, &BTreeMap::new())
            .expect("the defaults load");
        let report = check_report(&loaded).expect("the tree renders");
        assert!(report.starts_with("# file: none\n"), "{report}");
        assert!(
            report.contains("server.listen = \"\"  # default\n"),
            "{report}"
        );
    }
}
