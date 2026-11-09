// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The refusal of a key a configuration tree no longer reads.
//!
//! A section keeps each retired key as a [`Removed`] field, so the strict
//! load still accepts it, and refuses it after the load through
//! [`Removals`], which names the replacement of every retired key the
//! sources set, all of them at once. An old file therefore never starts
//! with a default in the place of what its operator wrote, and a key is
//! never read under two names (`Loader::rename` reads an old name as the
//! new one, and is not used for this). No specification governs this: our
//! own design.
//!
//! ```
//! use ferrohealth_config::removed::{Removals, Removed};
//!
//! #[derive(Default, serde::Deserialize)]
//! #[serde(default, deny_unknown_fields)]
//! struct Server {
//!     request_timeout: Option<String>,
//!     request_timeout_ms: Removed,
//! }
//!
//! let server: Server = toml::from_str("request_timeout_ms = 30000").unwrap();
//! let mut removals = Removals::new();
//! let (old, new) = ("server.request_timeout_ms", "server.request_timeout");
//! removals.replaced(server.request_timeout_ms, old, format!("{new}, a duration such as `30s`"));
//! let refusal = removals.finish().unwrap_err().to_string();
//! assert!(refusal.starts_with("server.request_timeout_ms was replaced by server.request_timeout"));
//! ```

use std::fmt;

use serde::de::{Deserializer, IgnoredAny};
use serde::{Deserialize, Serialize, Serializer};

/// A retired key: whether the sources set it, whatever its value.
///
/// It reads any value and keeps none of it, so a refusal can never show
/// one, and it renders as absent, so `config check` never shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Removed {
    set: bool,
}

impl Removed {
    /// Whether the sources set the key.
    #[must_use]
    pub const fn is_set(self) -> bool {
        self.set
    }
}

impl<'de> Deserialize<'de> for Removed {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self { set: true })
    }
}

impl Serialize for Removed {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_none()
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for Removed {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Removed")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ferrohealth_config::removed::Removed")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "not": {},
            "deprecated": true,
            "description": "A retired key: setting it refuses the configuration, naming its replacement."
        })
    }
}

/// One retired key the sources set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    key: String,
    replacement: Replacement,
}

/// What took a retired key's place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Replacement {
    /// The key or keys to write instead, with the form of their value.
    By(String),
    /// Nothing: why the key went.
    None(String),
}

impl Removal {
    /// The dotted key that was set.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// What to write instead, when anything takes its place.
    #[must_use]
    pub fn replacement(&self) -> Option<&str> {
        match &self.replacement {
            Replacement::By(replacement) => Some(replacement),
            Replacement::None(_) => None,
        }
    }
}

impl fmt::Display for Removal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.replacement {
            Replacement::By(replacement) => {
                write!(formatter, "{} was replaced by {replacement}", self.key)
            }
            Replacement::None(reason) => {
                write!(
                    formatter,
                    "{} was removed with no replacement: {reason}",
                    self.key
                )
            }
        }
    }
}

/// The retired keys a configuration set, gathered over every section
/// before any is refused.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removals {
    found: Vec<Removal>,
}

impl Removals {
    /// Returns an empty gathering.
    #[must_use]
    pub const fn new() -> Self {
        Self { found: Vec::new() }
    }

    /// Records `key` when `removed` is set: it was replaced by
    /// `replacement`, the key to write instead and the form of its value.
    pub fn replaced(
        &mut self,
        removed: Removed,
        key: impl Into<String>,
        replacement: impl Into<String>,
    ) {
        if removed.is_set() {
            self.found.push(Removal {
                key: key.into(),
                replacement: Replacement::By(replacement.into()),
            });
        }
    }

    /// Records `key` when `removed` is set: it was retired with nothing in
    /// its place, for `reason`.
    pub fn retired(&mut self, removed: Removed, key: impl Into<String>, reason: impl Into<String>) {
        if removed.is_set() {
            self.found.push(Removal {
                key: key.into(),
                replacement: Replacement::None(reason.into()),
            });
        }
    }

    /// Adds every removal `other` gathered, as a section's own check
    /// returns them.
    pub fn extend(&mut self, other: Self) {
        self.found.extend(other.found);
    }

    /// Whether no retired key was set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }

    /// Every removal gathered, in the order they were checked.
    #[must_use]
    pub fn removals(&self) -> &[Removal] {
        &self.found
    }

    /// Ends the gathering.
    ///
    /// # Errors
    ///
    /// Returns [`RemovedKeys`], naming every retired key that was set, when
    /// any was.
    pub fn finish(self) -> Result<(), RemovedKeys> {
        if self.found.is_empty() {
            Ok(())
        } else {
            Err(RemovedKeys {
                removals: self.found,
            })
        }
    }
}

/// A configuration that sets retired keys, each named with what replaced
/// it, one per line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", Lines(removals))]
pub struct RemovedKeys {
    removals: Vec<Removal>,
}

impl RemovedKeys {
    /// Every retired key that was set.
    #[must_use]
    pub fn removals(&self) -> &[Removal] {
        &self.removals
    }
}

/// Renders removals one per line.
struct Lines<'a>(&'a [Removal]);

impl fmt::Display for Lines<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, removal) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            write!(formatter, "{removal}")?;
        }
        Ok(())
    }
}
