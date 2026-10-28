// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The loader: the defaults, the file and the environment, read into one
//! typed tree.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{ConfigError, mask_values, position};
use crate::overrides::{Override, Placement, retype};
use crate::provenance::Provenance;
use crate::strict::did_you_mean;

/// The loader of one product's configuration.
///
/// `prefix` names the product's environment namespace: with `FERROSYS`, the
/// variable `FERROSYS__SERVER__LISTEN` sets `[server] listen` and
/// `FERROSYS_CONFIG` names the file. An override that names no key is
/// refused; every other variable that starts with `FERROSYS_` is left alone,
/// because product prefixes nest (`FERROTERM` and `FERROTERM_CONSOLE`).
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
    defaults: toml::Table,
}

/// A transform of an alias's value, such as a port into a listen address.
pub type Transform = fn(&str) -> String;

/// A key's value composed from several variables of the environment, such
/// as a database URL from the libpq set; `None` when they do not give one.
pub type Compose = fn(&BTreeMap<String, String>) -> Option<String>;

/// A variable outside the uniform form, or a set of them, that sets one key.
#[derive(Debug, Clone)]
struct Alias {
    /// The name the provenance and the refusals give the source.
    variable: String,
    /// Every variable the alias reads.
    reads: Vec<String>,
    key: String,
    /// Whether the canonical override of the key wins over it.
    below: bool,
    value: AliasValue,
}

/// How an alias turns the environment into the key's value.
#[derive(Debug, Clone, Copy)]
enum AliasValue {
    /// The one variable's value, transformed when a transform is given.
    Variable(Option<Transform>),
    /// The value composed from every variable the alias reads.
    Composed(Compose),
}

impl Alias {
    /// The alias of the one variable `variable`.
    fn variable(variable: String, key: String, below: bool, transform: Option<Transform>) -> Self {
        Self {
            reads: vec![variable.clone()],
            variable,
            key,
            below,
            value: AliasValue::Variable(transform),
        }
    }

    /// The value this alias sets from `environment`, when it sets one.
    fn value(&self, environment: &BTreeMap<String, String>) -> Option<String> {
        match self.value {
            AliasValue::Variable(transform) => environment
                .get(&self.variable)
                .map(|raw| transform.map_or_else(|| raw.clone(), |transform| transform(raw))),
            AliasValue::Composed(compose) => compose(environment),
        }
    }
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
///
/// Its `Debug` rendering shows the tree, the provenance and the
/// deprecations, and never the table of [`Loaded::table_with_secrets`].
#[derive(Clone)]
pub struct Loaded<T> {
    settings: T,
    provenance: Provenance,
    deprecations: Vec<Deprecation>,
    table: toml::Table,
}

impl<T: fmt::Debug> fmt::Debug for Loaded<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Loaded")
            .field("settings", &self.settings)
            .field("provenance", &self.provenance)
            .field("deprecations", &self.deprecations)
            .finish_non_exhaustive()
    }
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

    /// Returns the table the sources set, which holds every secret as read.
    ///
    /// It is what the file, the environment and the `--set` pairs set,
    /// merged as the load applied them: every renamed key under its new
    /// name, and every override typed as the tree took it, a number, a
    /// boolean or an array where the tree wants one and text elsewhere.
    /// The defaults are left out, the tree's and those of
    /// [`Loader::defaults`] alike, so a key no source set is absent. Reading
    /// the table again as the file of [`Loader::load_sources`] with no
    /// environment gives the same tree.
    ///
    /// A secret set inline stands here as the text it was set to. Treat the
    /// table as sensitive: never log it or render it, and keep a secret out
    /// of anything stored from it, as its `<key>_file` sibling. Nothing in
    /// this crate logs or renders it.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeMap;
    ///
    /// use ferrohealth_config::loader::Loader;
    ///
    /// #[derive(Debug, Default, serde::Deserialize)]
    /// #[serde(default, deny_unknown_fields)]
    /// struct Config {
    ///     server: Server,
    /// }
    ///
    /// #[derive(Debug, Default, serde::Deserialize)]
    /// #[serde(default, deny_unknown_fields)]
    /// struct Server {
    ///     listen: String,
    ///     body_limit_bytes: usize,
    /// }
    ///
    /// let environment = BTreeMap::from([(
    ///     "FERROSYS__SERVER__BODY_LIMIT_BYTES".to_owned(),
    ///     "2048".to_owned(),
    /// )]);
    /// let loaded = Loader::new("FERROSYS")?.load_sources::<Config>(None, None, &environment)?;
    /// let server = loaded.table_with_secrets()["server"].as_table();
    /// assert_eq!(Some(2048), server.and_then(|s| s["body_limit_bytes"].as_integer()));
    /// assert!(server.is_some_and(|s| !s.contains_key("listen")), "a default is left out");
    /// # Ok::<(), ferrohealth_config::error::ConfigError>(())
    /// ```
    #[must_use]
    pub const fn table_with_secrets(&self) -> &toml::Table {
        &self.table
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
            defaults: toml::Table::new(),
        })
    }

    /// Layers `value` as the product's defaults for the dotted key `key`,
    /// the whole tree when `key` is empty, under the file, the environment
    /// and the `--set` pairs.
    ///
    /// A key no source sets takes the product's value in place of the
    /// tree's `Default`, key by key, so a partial table keeps every product
    /// default it does not name, and the provenance reports the key as
    /// [`Source::Default`](crate::provenance::Source::Default). A key both
    /// this call and an earlier one set takes this call's value. A key
    /// `value` does not serialize, such as an `Option` that is `None`, falls
    /// back to the tree's `Default`. A secret serializes as its redaction
    /// marker, so a secret takes no product default.
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
    /// // The section of a shared crate, with the shared crate's defaults.
    /// #[derive(Debug, serde::Deserialize, serde::Serialize)]
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
    /// let product = Server { listen: "0.0.0.0:8080".to_owned(), body_limit_bytes: 64 << 20 };
    /// let loader = Loader::new("FERROSYS")?.defaults("server", &product)?;
    /// let loaded = loader.load_sources::<Config>(
    ///     Some("[server]\nlisten = \"10.0.0.5:8080\"\n"),
    ///     None,
    ///     &BTreeMap::new(),
    /// )?;
    /// assert_eq!("10.0.0.5:8080", loaded.settings().server.listen);
    /// assert_eq!(64 << 20, loaded.settings().server.body_limit_bytes);
    /// assert_eq!(Source::Default, loaded.provenance().source("server.body_limit_bytes"));
    /// # Ok::<(), ferrohealth_config::error::ConfigError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Assemble`] when `value` does not serialize to
    /// a TOML table.
    pub fn defaults<D: Serialize + ?Sized>(
        mut self,
        key: &str,
        value: &D,
    ) -> Result<Self, ConfigError> {
        let mut table = match toml::Value::try_from(value) {
            Ok(toml::Value::Table(table)) => table,
            Ok(_) => {
                return Err(ConfigError::Assemble {
                    source: <toml::ser::Error as serde::ser::Error>::custom(
                        "the defaults of a key are a table",
                    ),
                });
            }
            Err(source) => return Err(ConfigError::Assemble { source }),
        };
        if !key.is_empty() {
            for segment in key.rsplit('.') {
                table = toml::Table::from_iter([(segment.to_owned(), toml::Value::Table(table))]);
            }
        }
        overlay(&mut self.defaults, table);
        Ok(self)
    }

    /// Adds `variable` as an alias that sets the dotted key `key`.
    ///
    /// An alias is applied after every `<PREFIX>__` override, so it wins
    /// over the file and over the override of the same key: it is the name an
    /// operator sets for one run, such as `RUST_LOG` for `telemetry.filter`.
    #[must_use]
    pub fn alias(mut self, variable: impl Into<String>, key: impl Into<String>) -> Self {
        self.aliases
            .push(Alias::variable(variable.into(), key.into(), false, None));
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
        self.aliases.push(Alias::variable(
            variable.into(),
            key.into(),
            true,
            transform,
        ));
        self
    }

    /// Adds a fallback for the dotted key `key` composed from the variables
    /// `variables` by `compose`, such as [`crate::libpq::url`] for a
    /// database URL from the libpq set.
    ///
    /// It sits among the fallbacks in the order they were added, so a later
    /// fallback of the same key wins over it, and the canonical `<PREFIX>__`
    /// override wins over every fallback. The provenance names the variables
    /// joined by `+`, and no refusal quotes the composed value, as for any other override.
    #[must_use]
    pub fn fallback_composed(
        mut self,
        variables: &[&str],
        key: impl Into<String>,
        compose: Compose,
    ) -> Self {
        self.aliases.push(Alias {
            variable: variables.join("+"),
            reads: variables.iter().map(|&name| name.to_owned()).collect(),
            key: key.into(),
            below: true,
            value: AliasValue::Composed(compose),
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
    /// itself and that sets no key.
    ///
    /// The loader leaves every name outside the `<PREFIX>__` override form
    /// alone, so a reserved name needs no declaration to pass; it records
    /// which names the product owns.
    #[must_use]
    pub fn reserve(mut self, variable: impl Into<String>) -> Self {
        self.reserved.push(variable.into());
        self
    }

    /// Returns the names [`Loader::reserve`] recorded, in the order given.
    #[must_use]
    pub fn reserved(&self) -> &[String] {
        &self.reserved
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
            let mut filled = table.clone();
            fill(&mut filled, &self.defaults);
            // The merged tree is written and read again so every refusal
            // carries the position of its key, which a `Table` cannot report.
            let merged =
                toml::to_string(&filled).map_err(|source| ConfigError::Assemble { source })?;
            let error = match toml::from_str::<T>(&merged) {
                Ok(settings) if errors.is_empty() => {
                    warn_deprecated(&deprecations);
                    return Ok(Loaded {
                        settings,
                        provenance,
                        deprecations,
                        table,
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
            alias
                .value(environment)
                .map(|value| Override::new(&alias.variable, &alias.key, &value))
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
                || self
                    .aliases
                    .iter()
                    .any(|alias| alias.reads.iter().any(|read| read == name));
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
            .and_then(|key| {
                provenance
                    .variable(key)
                    .or_else(|| provenance.variable_below(key))
            })
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

/// Puts every key of `above` into `below`, merging the tables both hold.
fn overlay(below: &mut toml::Table, above: toml::Table) {
    for (key, value) in above {
        match (below.get_mut(&key), value) {
            (Some(toml::Value::Table(inner)), toml::Value::Table(value)) => overlay(inner, value),
            (_, value) => {
                below.insert(key, value);
            }
        }
    }
}

/// Puts every key of `defaults` that `table` does not hold into it, into
/// the tables both hold too.
fn fill(table: &mut toml::Table, defaults: &toml::Table) {
    for (key, default) in defaults {
        match table.get_mut(key) {
            Some(toml::Value::Table(set)) => {
                if let toml::Value::Table(default) = default {
                    fill(set, default);
                }
            }
            Some(_) => {}
            None => {
                table.insert(key.clone(), default.clone());
            }
        }
    }
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

    fn product_defaults() -> Loader {
        let product = Server {
            listen: "0.0.0.0:8080".to_owned(),
            body_limit_bytes: 64 << 20,
            tls: true,
            origins: vec!["https://product.example".to_owned()],
        };
        loader()
            .defaults("server", &product)
            .expect("the defaults serialize")
    }

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
        let file =
            "[server]\nbind = \"10.0.0.5:8080\"\n[cdr]\nurl = \"https://cdr.example.org/\"\n";
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
    fn a_composed_fallback_reads_several_variables_in_the_fallback_order() {
        let composed = || {
            loader()
                .fallback_composed(crate::libpq::VARIABLES, "cdr.url", crate::libpq::url)
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
            Some(crate::libpq::VARIABLES.join("+").as_str()),
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
