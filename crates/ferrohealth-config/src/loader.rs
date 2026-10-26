// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The loader: the defaults, the file and the environment, read into one
//! typed tree.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::error::{ConfigError, mask_values, position};
use crate::overrides::{Override, Placement, retype};
use crate::provenance::Provenance;
use crate::strict::{did_you_mean, sweep};

/// The loader of one product's configuration.
///
/// `prefix` names the product's environment namespace: with `FERROSYS`, the
/// variable `FERROSYS__SERVER__LISTEN` sets `[server] listen` and
/// `FERROSYS_CONFIG` names the file. Every other variable that starts with
/// `FERROSYS_` must be an alias or a reserved name, or the load is refused.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use ferrohealth_config::loader::Loader;
/// use ferrohealth_config::provenance::Source;
///
/// #[derive(Debug, Default, serde::Deserialize)]
/// #[serde(default, deny_unknown_fields)]
/// struct Config {
///     server: Server,
/// }
///
/// #[derive(Debug, serde::Deserialize)]
/// #[serde(default, deny_unknown_fields)]
/// struct Server {
///     listen: String,
///     body_limit_bytes: usize,
/// }
///
/// impl Default for Server {
///     fn default() -> Self {
///         Self { listen: "127.0.0.1:8080".to_owned(), body_limit_bytes: 1_048_576 }
///     }
/// }
///
/// let loader = Loader::new("FERROSYS")?;
/// let environment = BTreeMap::from([(
///     "FERROSYS__SERVER__BODY_LIMIT_BYTES".to_owned(),
///     "2048".to_owned(),
/// )]);
/// let loaded = loader.load_sources::<Config>(
///     Some("[server]\nlisten = \"0.0.0.0:8080\"\n"),
///     None,
///     &environment,
/// )?;
/// assert_eq!("0.0.0.0:8080", loaded.settings().server.listen);
/// assert_eq!(2048, loaded.settings().server.body_limit_bytes);
/// assert_eq!(Source::Environment, loaded.provenance().source("server.body_limit_bytes"));
/// # Ok::<(), ferrohealth_config::error::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Loader {
    prefix: String,
    aliases: Vec<Alias>,
    reserved: Vec<String>,
    renames: Vec<Deprecation>,
}

/// A transform of an alias's value, such as a port into a listen address.
pub type Transform = fn(&str) -> String;

/// A variable outside the uniform form that sets one key.
#[derive(Debug, Clone)]
struct Alias {
    variable: String,
    key: String,
    /// Whether the canonical override of the key wins over it.
    below: bool,
    transform: Option<Transform>,
}

/// A key renamed from one release to the next, still read under its old
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    /// The old dotted key.
    pub old: String,
    /// The new dotted key.
    pub new: String,
    /// The release that refuses the old key.
    pub refused_in: String,
}

/// The most refusals one load reports.
pub const MAX_REFUSALS: usize = 16;

/// A tree the loader read, with where each key came from.
#[derive(Debug, Clone)]
pub struct Loaded<T> {
    settings: T,
    provenance: Provenance,
    deprecations: Vec<Deprecation>,
}

impl<T> Loaded<T> {
    /// Returns the tree.
    #[must_use]
    pub const fn settings(&self) -> &T {
        &self.settings
    }

    /// Returns where each key came from.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Returns the renamed keys this configuration still set under their
    /// old name.
    #[must_use]
    pub fn deprecations(&self) -> &[Deprecation] {
        &self.deprecations
    }

    /// Returns the tree and its provenance.
    #[must_use]
    pub fn into_parts(self) -> (T, Provenance) {
        (self.settings, self.provenance)
    }
}

impl Loader {
    /// Creates the loader of the namespace `prefix`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Prefix`] unless `prefix` is upper-case ASCII
    /// letters and digits in words joined by single underscores, starting
    /// with a letter.
    pub fn new(prefix: impl Into<String>) -> Result<Self, ConfigError> {
        let prefix = prefix.into();
        let words_are_valid = prefix.split('_').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        });
        let starts_with_letter = prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase());
        if !words_are_valid || !starts_with_letter {
            return Err(ConfigError::Prefix { prefix });
        }
        Ok(Self {
            prefix,
            aliases: Vec::new(),
            reserved: Vec::new(),
            renames: Vec::new(),
        })
    }

    /// Adds `variable` as an alias that sets the dotted key `key`.
    ///
    /// An alias is applied after every `<PREFIX>__` override, so it wins
    /// over the file and over the override of the same key: it is the name an
    /// operator sets for one run, such as `RUST_LOG` for `telemetry.filter`.
    #[must_use]
    pub fn alias(mut self, variable: impl Into<String>, key: impl Into<String>) -> Self {
        self.aliases.push(Alias {
            variable: variable.into(),
            key: key.into(),
            below: false,
            transform: None,
        });
        self
    }

    /// Adds `variable` as a fallback for the dotted key `key`: it wins over
    /// the file, and the canonical `<PREFIX>__` override of the key wins
    /// over it. It is the shape of a name every platform sets, such as
    /// `DATABASE_URL`. `transform`, when given, turns the variable's value
    /// into the key's, such as `PORT` into `0.0.0.0:<PORT>`.
    #[must_use]
    pub fn fallback(
        mut self,
        variable: impl Into<String>,
        key: impl Into<String>,
        transform: Option<Transform>,
    ) -> Self {
        self.aliases.push(Alias {
            variable: variable.into(),
            key: key.into(),
            below: true,
            transform,
        });
        self
    }

    /// Reads the dotted key `old` as `new` until the release `refused_in`:
    /// the value moves, the load reports it in [`Loaded::deprecations`] and
    /// logs it, and both set at once is refused.
    #[must_use]
    pub fn rename(
        mut self,
        old: impl Into<String>,
        new: impl Into<String>,
        refused_in: impl Into<String>,
    ) -> Self {
        self.renames.push(Deprecation {
            old: old.into(),
            new: new.into(),
            refused_in: refused_in.into(),
        });
        self
    }

    /// Reserves `variable`, a name under the prefix the product reads
    /// itself and that sets no key, so the sweep passes it.
    #[must_use]
    pub fn reserve(mut self, variable: impl Into<String>) -> Self {
        self.reserved.push(variable.into());
        self
    }

    /// Returns the namespace prefix.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Returns the variable that names the configuration file,
    /// `<PREFIX>_CONFIG`.
    #[must_use]
    pub fn config_variable(&self) -> String {
        format!("{}_CONFIG", self.prefix)
    }

    /// Reads the configuration of the running process: the file `path`
    /// names, or the one `<PREFIX>_CONFIG` names, and the process
    /// environment.
    ///
    /// Without either path, only the defaults and the environment stand.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NotUnicode`] for a variable this loader reads
    /// whose value is not UTF-8, [`ConfigError::Read`] when the named file
    /// cannot be read, and every error of [`Loader::load_sources`].
    pub fn load<T: DeserializeOwned>(&self, path: Option<&Path>) -> Result<Loaded<T>, ConfigError> {
        self.load_with(path, &[])
    }

    /// Reads the configuration of the running process as [`Loader::load`]
    /// does, with the `--set key=value` pairs of `sets` applied last.
    ///
    /// # Errors
    ///
    /// Returns every error of [`Loader::load`].
    pub fn load_with<T: DeserializeOwned>(
        &self,
        path: Option<&Path>,
        sets: &[(String, String)],
    ) -> Result<Loaded<T>, ConfigError> {
        let environment = self.environment(std::env::vars_os())?;
        let named = path
            .map(Path::to_path_buf)
            .or_else(|| environment.get(&self.config_variable()).map(PathBuf::from));
        let text = match named.as_deref() {
            None => None,
            Some(path) => {
                Some(
                    std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
                        path: path.to_path_buf(),
                        source,
                    })?,
                )
            }
        };
        self.load_sources_with(text.as_deref(), named.as_deref(), &environment, sets)
    }

    /// Reads `text` as the TOML file (read from `path`, when it came from a
    /// file) and applies `environment` over it.
    ///
    /// Reading the sources and reading the process are separate so a test
    /// states both: setting a variable is `unsafe` in edition 2024.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::Syntax`] when `text` is not TOML;
    /// - [`ConfigError::UnknownVariable`] for a variable under the prefix
    ///   that is neither an override, an alias nor reserved, or
    ///   [`ConfigError::Refusals`] naming every one when there are several;
    /// - [`ConfigError::OverrideName`] and [`ConfigError::OverrideShape`] for
    ///   an override that names no key;
    /// - [`ConfigError::Invalid`] for an unknown key or a value the tree
    ///   refuses, naming the key, the variable that set it and the closest
    ///   known key;
    /// - [`ConfigError::Assemble`] when the merged tree cannot be written.
    pub fn load_sources<T: DeserializeOwned>(
        &self,
        text: Option<&str>,
        path: Option<&Path>,
        environment: &BTreeMap<String, String>,
    ) -> Result<Loaded<T>, ConfigError> {
        self.load_sources_with(text, path, environment, &[])
    }

    /// Reads the sources as [`Loader::load_sources`] does, with the
    /// `--set key=value` pairs of `sets` applied last, over the environment.
    ///
    /// # Errors
    ///
    /// Returns every error of [`Loader::load_sources`]. Up to
    /// [`MAX_REFUSALS`] invalid keys are collected into one
    /// [`ConfigError::Refusals`], and a renamed key set under both its old
    /// and its new name is refused as [`ConfigError::Renamed`].
    pub fn load_sources_with<T: DeserializeOwned>(
        &self,
        text: Option<&str>,
        path: Option<&Path>,
        environment: &BTreeMap<String, String>,
        sets: &[(String, String)],
    ) -> Result<Loaded<T>, ConfigError> {
        let file = match text {
            None => toml::Table::new(),
            Some(text) => toml::from_str(text).map_err(|error| ConfigError::Syntax {
                position: error.span().and_then(|span| position(text, span.start)),
                reason: mask_values(error.message()),
            })?,
        };
        let config_variable = self.config_variable();
        let mut known: Vec<&str> = vec![config_variable.as_str()];
        known.extend(self.reserved.iter().map(String::as_str));
        known.extend(self.aliases.iter().map(|alias| alias.variable.as_str()));
        let mut refusals = sweep(&self.prefix, environment, &known);
        if refusals.len() > 1 {
            return Err(ConfigError::Refusals { refusals });
        }
        if let Some(refusal) = refusals.pop() {
            return Err(refusal);
        }
        let overrides = self.overrides(environment, sets)?;
        let mut set_by: BTreeMap<String, String> = BTreeMap::new();
        for item in &overrides {
            let key = self.renamed(&item.key());
            set_by.insert(key, item.name().to_owned());
        }
        let set_keys = sets.iter().map(|(key, _)| self.renamed(key)).collect();
        let provenance = Provenance::new(
            path.map(Path::to_path_buf),
            text.is_some().then_some(&file),
            set_by,
            set_keys,
        );
        let mut placements = BTreeMap::new();
        let mut dropped: Vec<Vec<String>> = Vec::new();
        let mut errors: Vec<ConfigError> = Vec::new();
        // Each pass places one more override, or drops one more refused key or
        // the table it left incomplete, so the loop ends after at most one pass
        // per override and per key.
        loop {
            let mut table = file.clone();
            for (index, item) in overrides.iter().enumerate() {
                item.apply(
                    &mut table,
                    placements.get(&index).copied().unwrap_or(Placement::Text),
                )?;
            }
            let deprecations = self.apply_renames(&mut table)?;
            for path in &dropped {
                remove(&mut table, path);
            }
            // The merged tree is written and read again so every refusal
            // carries the position of its key, which a `Table` cannot report.
            let merged =
                toml::to_string(&table).map_err(|source| ConfigError::Assemble { source })?;
            let error = match toml::from_str::<T>(&merged) {
                Ok(settings) if errors.is_empty() => {
                    warn_deprecated(&deprecations);
                    return Ok(Loaded {
                        settings,
                        provenance,
                        deprecations,
                    });
                }
                Ok(_) => return Err(collected(errors)),
                Err(error) => error,
            };
            let span = error.span();
            let wants_sequence = error.message().contains("expected a sequence");
            if let Some((index, placement)) = span
                .as_ref()
                .and_then(|span| retype(&overrides, &placements, &merged, span, wants_sequence))
            {
                placements.insert(index, placement);
                continue;
            }
            let (refusal, key) = invalid(&error, &merged, &provenance);
            // NOTE: no specification governs this: our own design; a field missing only
            // because its refused value was dropped is no refusal, so its table goes too.
            if follows_a_drop(&error, key.as_deref(), &dropped) {
                match key {
                    Some(table) => {
                        dropped.push(table.split('.').map(str::to_owned).collect());
                        continue;
                    }
                    None => return Err(collected(errors)),
                }
            }
            errors.push(refusal);
            match key {
                Some(key) if errors.len() < MAX_REFUSALS => {
                    dropped.push(key.split('.').map(str::to_owned).collect());
                }
                _ => return Err(collected(errors)),
            }
        }
    }

    /// The overrides of `environment` and `sets`, in the order they apply:
    /// the fallbacks, the `<PREFIX>__` overrides, the aliases, the sets.
    fn overrides(
        &self,
        environment: &BTreeMap<String, String>,
        sets: &[(String, String)],
    ) -> Result<Vec<Override>, ConfigError> {
        let alias = |alias: &Alias| {
            environment.get(&alias.variable).map(|raw| {
                let value = alias
                    .transform
                    .map_or_else(|| raw.clone(), |transform| transform(raw));
                Override::new(&alias.variable, &alias.key, &value)
            })
        };
        let mut overrides: Vec<Override> = self
            .aliases
            .iter()
            .filter(|a| a.below)
            .filter_map(alias)
            .collect();
        for (name, raw) in environment {
            if let Some(item) = Override::parse(&self.prefix, name, raw)? {
                overrides.push(item);
            }
        }
        overrides.extend(self.aliases.iter().filter(|a| !a.below).filter_map(alias));
        overrides.extend(
            sets.iter()
                .map(|(key, raw)| Override::new(&format!("--set {key}"), key, raw)),
        );
        Ok(overrides)
    }

    /// The key `key` is read as: its new name when it was renamed.
    fn renamed(&self, key: &str) -> String {
        self.renames
            .iter()
            .find(|rename| rename.old == key)
            .map_or_else(|| key.to_owned(), |rename| rename.new.clone())
    }

    /// Moves every renamed key of `table` to its new name.
    fn apply_renames(&self, table: &mut toml::Table) -> Result<Vec<Deprecation>, ConfigError> {
        let mut found = Vec::new();
        for rename in &self.renames {
            let old: Vec<String> = rename.old.split('.').map(str::to_owned).collect();
            let new: Vec<String> = rename.new.split('.').map(str::to_owned).collect();
            let Some(value) = take(table, &old) else {
                continue;
            };
            if lookup(table, &new).is_some() {
                return Err(ConfigError::Renamed {
                    old: rename.old.clone(),
                    new: rename.new.clone(),
                });
            }
            insert(table, &new, value);
            found.push(rename.clone());
        }
        Ok(found)
    }

    /// Collects the variables this loader reads from the process
    /// environment: those under the prefix and the aliases.
    fn environment(
        &self,
        variables: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BTreeMap<String, String>, ConfigError> {
        let reserved = format!("{}_", self.prefix);
        let mut environment = BTreeMap::new();
        for (name, value) in variables {
            // NOTE: no specification governs this: our own design. A name that is
            // not UTF-8 cannot be one of this loader's, so it is legitimately skipped.
            let Some(name) = name.to_str() else {
                continue;
            };
            let read = name.starts_with(&reserved)
                || self.aliases.iter().any(|alias| alias.variable == name);
            if !read {
                continue;
            }
            // NOTE: no specification governs this: our own design. The error is the
            // value itself, which may be a secret, so the refusal names only the variable.
            let value = value
                .into_string()
                .map_err(|_value| ConfigError::NotUnicode {
                    name: name.to_owned(),
                })?;
            environment.insert(name.to_owned(), value);
        }
        Ok(environment)
    }
}

/// Returns the dotted key whose key or value holds the start of `span` in
/// the TOML text `merged`, the deepest one first.
fn key_at(merged: &str, span: &Range<usize>) -> Option<String> {
    let document = toml::de::DeTable::parse(merged).ok()?.into_inner();
    find_key(&document, span.start).map(|path| path.join("."))
}

/// Walks `table` for the deepest key that holds `at`.
fn find_key(table: &toml::de::DeTable<'_>, at: usize) -> Option<Vec<String>> {
    for (key, value) in table {
        let name = key.get_ref().to_string();
        if let Some(inner) = value.get_ref().as_table()
            && let Some(mut below) = find_key(inner, at)
        {
            below.insert(0, name);
            return Some(below);
        }
        if key.span().contains(&at) || value.span().contains(&at) {
            return Some(vec![name]);
        }
    }
    None
}

/// The refusal of `error`, read from the merged text, and the dotted key it
/// names.
fn invalid(
    error: &toml::de::Error,
    merged: &str,
    provenance: &Provenance,
) -> (ConfigError, Option<String>) {
    let key = error.span().as_ref().and_then(|span| key_at(merged, span));
    let suggestion =
        suggest(error.message()).map(|(name, field)| match key.as_deref().and_then(parent) {
            Some(parent) if field => format!("{parent}.{name}"),
            _ => name,
        });
    let refusal = ConfigError::Invalid {
        variable: key
            .as_deref()
            .and_then(|key| provenance.variable(key))
            .map(str::to_owned),
        key: key.clone(),
        suggestion,
        reason: mask_values(error.message()),
    };
    (refusal, key)
}

/// Whether `error` reports a field missing only because a refused key was
/// dropped to look for further refusals: `missing field` under the table at
/// `key` (the root when `None`), naming a dropped key.
fn follows_a_drop(error: &toml::de::Error, key: Option<&str>, dropped: &[Vec<String>]) -> bool {
    let Some(field) = error
        .message()
        .split_once("missing field `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(field, _)| field)
    else {
        return false;
    };
    let mut path: Vec<String> = key
        .map(|key| key.split('.').map(str::to_owned).collect())
        .unwrap_or_default();
    path.push(field.to_owned());
    dropped.contains(&path)
}

/// The one error of `errors`, or all of them together.
fn collected(mut errors: Vec<ConfigError>) -> ConfigError {
    if errors.len() == 1 {
        errors.remove(0)
    } else {
        ConfigError::Refusals { refusals: errors }
    }
}

/// Logs every renamed key the configuration still sets under its old name.
fn warn_deprecated(deprecations: &[Deprecation]) {
    for deprecation in deprecations {
        tracing::warn!(
            old = deprecation.old.as_str(),
            new = deprecation.new.as_str(),
            refused_in = deprecation.refused_in.as_str(),
            "a renamed configuration key is set under its old name"
        );
    }
}

/// Returns the value at `path` in `table`.
fn lookup<'a>(table: &'a toml::Table, path: &[String]) -> Option<&'a toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut cursor = table;
    for parent in parents {
        cursor = cursor.get(parent.as_str())?.as_table()?;
    }
    cursor.get(last.as_str())
}

/// Removes and returns the value at `path` in `table`.
fn take(table: &mut toml::Table, path: &[String]) -> Option<toml::Value> {
    let (last, parents) = path.split_last()?;
    let mut cursor = table;
    for parent in parents {
        cursor = cursor.get_mut(parent.as_str())?.as_table_mut()?;
    }
    cursor.remove(last.as_str())
}

/// Removes the value at `path` in `table`, when there is one.
fn remove(table: &mut toml::Table, path: &[String]) {
    // NOTE: no specification governs this: our own design; a refused key that is
    // already gone has nothing left to remove.
    let _removed: Option<toml::Value> = take(table, path);
}

/// Puts `value` at `path` in `table`, creating the tables above it.
fn insert(table: &mut toml::Table, path: &[String], value: toml::Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = table;
    for parent in parents {
        let entry = cursor
            .entry(parent.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let toml::Value::Table(next) = entry else {
            return;
        };
        cursor = next;
    }
    cursor.insert(last.clone(), value);
}

/// Returns the dotted key above `key`, or `None` for a top-level key.
fn parent(key: &str) -> Option<&str> {
    key.rsplit_once('.').map(|(parent, _)| parent)
}

/// Returns the closest expected name for serde's refusal of an unknown
/// field or variant (``unknown field `x`, expected one of `a`, `b` ``), and
/// whether it names a field.
fn suggest(message: &str) -> Option<(String, bool)> {
    let field = message.starts_with("unknown field");
    if !(field || message.starts_with("unknown variant")) {
        return None;
    }
    let mut names = message.split('`').skip(1).step_by(2);
    let unknown = names.next()?;
    let expected: Vec<&str> = names.collect();
    did_you_mean(unknown, &expected).map(|name| (name, field))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io::Write as _;

    use ferrohealth_secret::secret::Secret;

    use super::{Loader, suggest};
    use crate::error::ConfigError;
    use crate::provenance::Source;

    const SENTINEL: &str = "Qz7sentinel";

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Config {
        spec_profile: String,
        server: Server,
        telemetry: Telemetry,
        cdr: Option<Cdr>,
    }

    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Server {
        listen: String,
        body_limit_bytes: usize,
        tls: bool,
        origins: Vec<String>,
    }

    impl Default for Server {
        fn default() -> Self {
            Self {
                listen: "127.0.0.1:8080".to_owned(),
                body_limit_bytes: 1024,
                tls: false,
                origins: Vec::new(),
            }
        }
    }

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default, deny_unknown_fields)]
    struct Telemetry {
        format: Format,
        filter: String,
    }

    #[derive(Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
    #[serde(rename_all = "lowercase")]
    enum Format {
        #[default]
        Auto,
        Json,
        Pretty,
    }

    #[derive(Debug, serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Cdr {
        url: String,
        user: Option<String>,
        password: Option<Secret>,
        password_file: Option<std::path::PathBuf>,
    }

    fn loader() -> Loader {
        Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .alias("RUST_LOG", "telemetry.filter")
            .alias("FERROSYS_LOG_FORMAT", "telemetry.format")
    }

    fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
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
            matches!(&error, ConfigError::Invalid { suggestion: Some(suggestion), .. } if suggestion == "server"),
            "{error:?}"
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
    fn an_unread_variable_under_the_prefix_is_refused() {
        let error = loader()
            .load_sources::<Config>(
                None,
                None,
                &environment(&[("FERROSYS_SERVER__LISTEN", "x")]),
            )
            .expect_err("the single-underscore form is no override");
        assert!(
            matches!(&error, ConfigError::UnknownVariable { suggestion: Some(suggestion), .. } if suggestion == "FERROSYS__SERVER__LISTEN"),
            "{error:?}"
        );
        let error = loader()
            .load_sources::<Config>(
                None,
                None,
                &environment(&[("FERROSYS_BOGUS", "1"), ("FERROSYS_LOG_FORMT", "json")]),
            )
            .expect_err("both are refused");
        let ConfigError::Refusals { refusals } = &error else {
            panic!("an unexpected refusal: {error:?}");
        };
        assert_eq!(2, refusals.len(), "{error}");
        assert!(
            error
                .to_string()
                .contains("did you mean `FERROSYS_LOG_FORMAT`?"),
            "{error}"
        );
    }

    #[test]
    fn the_config_variable_and_a_reserved_name_pass_the_sweep() {
        let loaded = loader()
            .reserve("FERROSYS_STATE_DIR")
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
    fn the_process_environment_keeps_only_the_variables_the_loader_reads() {
        let variables = vec![
            (
                OsString::from("FERROSYS__SERVER__LISTEN"),
                OsString::from("0.0.0.0:1"),
            ),
            (OsString::from("RUST_LOG"), OsString::from("debug")),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
        ];
        let environment = loader()
            .environment(variables.into_iter())
            .expect("UTF-8 values");
        assert_eq!(
            vec!["FERROSYS__SERVER__LISTEN", "RUST_LOG"],
            environment.keys().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_the_loader_reads_must_be_utf8() {
        use std::os::unix::ffi::OsStringExt as _;
        let variables = vec![(
            OsString::from("FERROSYS__SERVER__LISTEN"),
            OsString::from_vec(vec![0xff, 0xfe]),
        )];
        let error = loader()
            .environment(variables.into_iter())
            .expect_err("bytes that are no UTF-8 are refused");
        assert!(matches!(error, ConfigError::NotUnicode { .. }), "{error:?}");
        let ignored = vec![(OsString::from("OTHER"), OsString::from_vec(vec![0xff]))];
        assert!(
            loader()
                .environment(ignored.into_iter())
                .expect("a foreign variable is skipped")
                .is_empty()
        );
    }

    #[test]
    fn a_named_file_that_cannot_be_read_is_refused() {
        let error = loader()
            .load::<Config>(Some(std::path::Path::new("/nonexistent/ferrosys.toml")))
            .expect_err("a missing file is refused");
        assert!(matches!(error, ConfigError::Read { .. }), "{error:?}");
    }

    #[test]
    fn a_suggestion_reads_serde_s_refusal() {
        assert_eq!(
            Some(("listen".to_owned(), true)),
            suggest("unknown field `listn`, expected one of `listen`, `tls`")
        );
        assert_eq!(
            Some(("json".to_owned(), false)),
            suggest("unknown variant `jsn`, expected `json` or `pretty`")
        );
        assert_eq!(None, suggest("unknown field `x`, there are no fields"));
        assert_eq!(None, suggest("invalid type: string \"x\", expected usize"));
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
            vec![super::Deprecation {
                old: "server.bind".to_owned(),
                new: "server.listen".to_owned(),
                refused_in: "0.3.0".to_owned(),
            }],
            loaded.deprecations()
        );
        let report = crate::report::check_report(&loaded).expect("the tree renders");
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
}
