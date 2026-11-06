// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The loader: the defaults, the file and the environment, read into one
//! typed tree.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{ConfigError, mask_values, position};
use crate::overrides::{Growth, Override, Placement, placements, retype};
use crate::provenance::Provenance;

mod environment;
mod refusal;
mod table;

use refusal::{collected, follows_a_drop, invalid};
use table::{dropped_path, fill, insert, lookup, overlay, remove, take};

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
        // NOTE: no specification governs this: our own design; the sets apply last,
        // so they are the last overrides, each key in the form a refusal names.
        let set_keys = overrides
            .iter()
            .rev()
            .take(sets.len())
            .map(|item| self.renamed(&item.key()))
            .collect();
        let provenance = Provenance::new(
            path.map(Path::to_path_buf),
            text.is_some().then_some(&file),
            set_by,
            set_keys,
        );
        let mut placements = placements(&overrides);
        let mut dropped: Vec<Vec<String>> = Vec::new();
        let mut errors: Vec<ConfigError> = Vec::new();
        // Each pass places one more override, or drops one more refused key or
        // the table it left incomplete, so the loop ends after at most one pass
        // per override and per key.
        loop {
            let mut table = file.clone();
            let mut growth = Growth::default();
            for (index, item) in overrides.iter().enumerate() {
                item.apply(
                    &mut table,
                    placements.get(&index).copied().unwrap_or(Placement::Text),
                    &mut growth,
                )?;
            }
            growth.check()?;
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
                        dropped.push(dropped_path(&table));
                        continue;
                    }
                    None => return Err(collected(errors)),
                }
            }
            errors.push(refusal);
            match key {
                Some(key) if errors.len() < MAX_REFUSALS => {
                    dropped.push(dropped_path(&key));
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
            .collect::<Result<_, _>>()?;
        for (name, raw) in environment {
            if let Some(item) = Override::parse(&self.prefix, name, raw)? {
                overrides.push(item);
            }
        }
        for item in self.aliases.iter().filter(|a| !a.below).filter_map(alias) {
            overrides.push(item?);
        }
        for (key, raw) in sets {
            overrides.push(Override::new(&format!("--set {key}"), key, raw)?);
        }
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
