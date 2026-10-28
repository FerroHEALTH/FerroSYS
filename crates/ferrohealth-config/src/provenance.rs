// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Where each key of a loaded configuration came from.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// The suffix of a secret's sibling key that names the file holding it.
const FILE_SUFFIX: &str = "_file";

/// The layer a key's value came from.
///
/// The variants are ordered by strength, `Default` the weakest and `Set` the
/// strongest, as the load applies the layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// No layer set it: the tree's default stands.
    Default,
    /// The configuration file.
    File,
    /// An environment variable.
    Environment,
    /// A `--set key=value` argument.
    Set,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Environment => "environment",
            Self::Set => "set",
        })
    }
}

/// The keys the file and the environment set, as the load read them.
///
/// A key counts as set when it, or a table above it, was set whole; a
/// `--set` argument wins over the environment, and the environment over the
/// file, as the load applies them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    file: Option<PathBuf>,
    file_keys: BTreeSet<String>,
    environment: BTreeMap<String, String>,
    sets: BTreeSet<String>,
}

impl Provenance {
    /// Returns the provenance of a load that read `file` (with its path, when
    /// it came from one) and set each key of `environment` from the variable
    /// it maps to.
    pub(crate) fn new(
        path: Option<PathBuf>,
        file: Option<&toml::Table>,
        environment: BTreeMap<String, String>,
        sets: BTreeSet<String>,
    ) -> Self {
        let mut file_keys = BTreeSet::new();
        if let Some(table) = file {
            leaves("", table, &mut file_keys);
        }
        Self {
            file: path,
            file_keys,
            environment,
            sets,
        }
    }

    /// Returns the file the load read, when it read one from a path.
    #[must_use]
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Returns where the value of the dotted key `key` came from.
    #[must_use]
    pub fn source(&self, key: &str) -> Source {
        if covers(&self.sets, key) {
            Source::Set
        } else if self.variable(key).is_some() {
            Source::Environment
        } else if covers(&self.file_keys, key) {
            Source::File
        } else {
            Source::Default
        }
    }

    /// Returns where the value of the secret at the dotted key `key` came
    /// from, counting the secret and its `<key>_file` sibling as one setting.
    ///
    /// `key` names either form, `oidc.client_secret` or
    /// `oidc.client_secret_file`, and both give the same answer: the
    /// stronger of the two keys' [`Provenance::source`], with `--set` over
    /// the environment, the environment over the file and the file over the
    /// default. It follows the `<key>_file` rule of
    /// `ferrohealth_secret::file`, so ask it only for a secret: for any
    /// other key whose name ends in `_file`, [`Provenance::source`] is the
    /// answer.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    /// use std::path::PathBuf;
    ///
    /// use ferrohealth_config::loader::Loader;
    /// use ferrohealth_config::provenance::Source;
    ///
    /// #[derive(Debug, Default, serde::Deserialize)]
    /// #[serde(default, deny_unknown_fields)]
    /// struct Config {
    ///     oidc: Oidc,
    /// }
    ///
    /// #[derive(Debug, Default, serde::Deserialize)]
    /// #[serde(default, deny_unknown_fields)]
    /// struct Oidc {
    ///     client_secret: Option<ferrohealth_secret::secret::Secret>,
    ///     client_secret_file: Option<PathBuf>,
    /// }
    ///
    /// let loaded = Loader::new("FERROSYS")?.load_sources::<Config>(
    ///     Some("[oidc]\nclient_secret_file = \"/run/secrets/oidc\"\n"),
    ///     None,
    ///     &BTreeMap::new(),
    /// )?;
    /// let provenance = loaded.provenance();
    /// assert_eq!(Source::Default, provenance.source("oidc.client_secret"));
    /// assert_eq!(Source::File, provenance.secret_source("oidc.client_secret"));
    /// assert_eq!(Source::File, provenance.secret_source("oidc.client_secret_file"));
    /// # Ok::<(), ferrohealth_config::error::ConfigError>(())
    /// ```
    #[must_use]
    pub fn secret_source(&self, key: &str) -> Source {
        let secret = key.strip_suffix(FILE_SUFFIX).unwrap_or(key);
        self.source(secret)
            .max(self.source(&format!("{secret}{FILE_SUFFIX}")))
    }

    /// Returns the environment variable that set the dotted key `key`, or a
    /// table above it, when one did.
    #[must_use]
    pub fn variable(&self, key: &str) -> Option<&str> {
        above(key)
            .find_map(|candidate| self.environment.get(candidate))
            .map(String::as_str)
    }
}

impl Provenance {
    /// Returns the first environment variable that set a key below the
    /// dotted key `key`, for a refusal of `key` itself, such as an unknown
    /// section a `<PREFIX>__SECTION__KEY` override created.
    pub(crate) fn variable_below(&self, key: &str) -> Option<&str> {
        let prefix = format!("{key}.");
        self.environment
            .iter()
            .find(|(set, _)| set.starts_with(&prefix))
            .map(|(_, variable)| variable.as_str())
    }
}

/// Returns `key` and every dotted key above it, the key itself first.
fn above(key: &str) -> impl Iterator<Item = &str> {
    std::iter::once(key).chain(key.rmatch_indices('.').filter_map(|(at, _)| key.get(..at)))
}

/// Returns whether `keys` holds `key` or a key above it.
fn covers(keys: &BTreeSet<String>, key: &str) -> bool {
    above(key).any(|candidate| keys.contains(candidate))
}

/// Adds to `out` the dotted key of every value under `table` that is not a
/// table, below `prefix`.
pub(crate) fn leaves(prefix: &str, table: &toml::Table, out: &mut BTreeSet<String>) {
    for (name, value) in table {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match value {
            toml::Value::Table(inner) => leaves(&key, inner, out),
            _ => {
                out.insert(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{Provenance, Source};

    #[test]
    fn the_environment_wins_over_the_file_and_a_whole_table_counts_for_its_keys() {
        let file: toml::Table = toml::from_str(
            "[server]\nlisten = \"0.0.0.0:8080\"\nbody_limit_bytes = 10\n[cdr.retry]\nmax_attempts = 2\n",
        )
        .expect("the file parses");
        let environment = BTreeMap::from([
            (
                "server.listen".to_owned(),
                "FERROSYS__SERVER__LISTEN".to_owned(),
            ),
            (
                "terminology.retry".to_owned(),
                "FERROSYS__TERMINOLOGY__RETRY".to_owned(),
            ),
        ]);
        let provenance = Provenance::new(
            Some(PathBuf::from("/etc/ferrosys.toml")),
            Some(&file),
            environment,
            std::collections::BTreeSet::from(["cdr.endpoint".to_owned()]),
        );
        assert_eq!(Source::Set, provenance.source("cdr.endpoint"));
        assert_eq!("set", Source::Set.to_string());
        assert_eq!(Source::Environment, provenance.source("server.listen"));
        assert_eq!(
            Some("FERROSYS__SERVER__LISTEN"),
            provenance.variable("server.listen")
        );
        assert_eq!(Source::File, provenance.source("server.body_limit_bytes"));
        assert_eq!(
            Source::Default,
            provenance.source("server.request_timeout_ms")
        );
        assert_eq!(Source::File, provenance.source("cdr.retry.max_attempts"));
        assert_eq!(
            Source::Default,
            provenance.source("cdr.retry.initial_backoff_ms")
        );
        assert_eq!(
            Source::Environment,
            provenance.source("terminology.retry.max_attempts")
        );
        assert_eq!(
            Some(std::path::Path::new("/etc/ferrosys.toml")),
            provenance.file()
        );
        assert_eq!("environment", Source::Environment.to_string());
    }

    #[test]
    fn a_load_without_a_file_or_an_environment_is_all_defaults() {
        let provenance = Provenance::default();
        assert_eq!(Source::Default, provenance.source("server.listen"));
        assert_eq!(None, provenance.file());
    }
}
