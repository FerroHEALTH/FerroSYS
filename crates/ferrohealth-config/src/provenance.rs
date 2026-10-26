// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Where each key of a loaded configuration came from.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// The layer a key's value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// No layer set it: the tree's default stands.
    Default,
    /// The configuration file.
    File,
    /// An environment variable.
    Environment,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "default",
            Self::File => "file",
            Self::Environment => "environment",
        })
    }
}

/// The keys the file and the environment set, as the load read them.
///
/// A key counts as set when it, or a table above it, was set whole; the
/// environment wins over the file, as the load applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    file: Option<PathBuf>,
    file_keys: BTreeSet<String>,
    environment: BTreeMap<String, String>,
}

impl Provenance {
    /// Returns the provenance of a load that read `file` (with its path, when
    /// it came from one) and set each key of `environment` from the variable
    /// it maps to.
    pub(crate) fn new(
        path: Option<PathBuf>,
        file: Option<&toml::Table>,
        environment: BTreeMap<String, String>,
    ) -> Self {
        let mut file_keys = BTreeSet::new();
        if let Some(table) = file {
            leaves("", table, &mut file_keys);
        }
        Self {
            file: path,
            file_keys,
            environment,
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
        if self.variable(key).is_some() {
            Source::Environment
        } else if covers(&self.file_keys, key) {
            Source::File
        } else {
            Source::Default
        }
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
        );
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
