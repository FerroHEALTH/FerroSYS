// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Every reason a configuration is refused.

use std::fmt;
use std::path::PathBuf;

use ferrohealth_secret::file::SecretError;

/// A configuration the product refuses to start on.
///
/// Every variant names the key, the variable or the file at fault; none
/// carries a value, so a refusal never shows a secret. The TOML reader's own
/// error is not kept as the cause of [`ConfigError::Syntax`] and
/// [`ConfigError::Invalid`]: its rendering quotes the offending line of the
/// document, which can hold a secret under a misspelt key, so the variants
/// keep its position and its reason with every quoted value masked.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The prefix the loader was built with is not upper-case ASCII letters,
    /// digits and single underscores.
    #[error(
        "`{prefix}` is no environment prefix; use upper-case letters, digits and single underscores"
    )]
    Prefix {
        /// The prefix that was given.
        prefix: String,
    },
    /// The configuration file could not be read.
    #[error("the configuration file {} could not be read", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// What the file system reported.
        #[source]
        source: std::io::Error,
    },
    /// The configuration file is not TOML.
    #[error("the configuration file is not TOML{}: {reason}", Position(*position))]
    Syntax {
        /// The 1-based line and column of the fault, when the reader names
        /// one.
        position: Option<(usize, usize)>,
        /// What the reader reported, every quoted value masked.
        reason: String,
    },
    /// The merged configuration names a key the tree does not have, or holds
    /// a value the tree refuses.
    #[error("{}", Refusal(self))]
    Invalid {
        /// The dotted key at fault, when the refusal names one.
        key: Option<String>,
        /// The environment variable that set the key, when one did.
        variable: Option<String>,
        /// The closest key the tree knows, for an unknown key.
        suggestion: Option<String>,
        /// What the reader reported, every quoted value masked.
        reason: String,
    },
    /// A variable this loader reads holds a value that is not UTF-8.
    #[error("{name} holds a value that is not UTF-8")]
    NotUnicode {
        /// The variable.
        name: String,
    },
    /// Several variables were refused at once; each is named.
    #[error("{}", Joined(refusals))]
    Refusals {
        /// Every refusal, sorted by variable name.
        refusals: Vec<ConfigError>,
    },
    /// An override names no section and key: `<PREFIX>__SECTION__KEY` needs
    /// both, each non-empty.
    #[error("{name} names no configuration key; use {prefix}__<SECTION>__<KEY>")]
    OverrideName {
        /// The variable.
        name: String,
        /// The override prefix without its separator.
        prefix: String,
    },
    /// An override addresses a key under a value that is not a section.
    #[error("{name} addresses a key under `{key}`, which is a value and no section")]
    OverrideShape {
        /// The variable.
        name: String,
        /// The dotted key that holds a value.
        key: String,
    },
    /// A variable under the prefix is neither an override, an alias nor a
    /// name the product reserved.
    #[error("{name} is no configuration variable{}", Suggestion(suggestion.as_deref()))]
    UnknownVariable {
        /// The variable.
        name: String,
        /// The spelling the variable probably meant.
        suggestion: Option<String>,
    },
    /// The merged tree could not be written back for reading.
    #[error("the configuration could not be assembled")]
    Assemble {
        /// What the TOML writer reported.
        #[source]
        source: toml::ser::Error,
    },
    /// The running configuration could not be rendered.
    #[error("the running configuration could not be rendered")]
    Render {
        /// What the TOML writer reported.
        #[source]
        source: toml::ser::Error,
    },
    /// A secret could not be read from its key or its `_file` sibling.
    #[error(transparent)]
    Secret(#[from] SecretError),
}

/// The messages of several refusals, one after another.
struct Joined<'a>(&'a [ConfigError]);

impl fmt::Display for Joined<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} configuration refusals", self.0.len())?;
        for refusal in self.0 {
            write!(f, "; {refusal}")?;
        }
        Ok(())
    }
}

/// The message of [`ConfigError::Invalid`].
struct Refusal<'a>(&'a ConfigError);

impl fmt::Display for Refusal<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ConfigError::Invalid {
            key,
            variable,
            suggestion,
            reason,
        } = self.0
        else {
            return Ok(());
        };
        match key {
            Some(key) => write!(f, "`{key}` is not valid: {reason}")?,
            None => write!(f, "the configuration is not valid: {reason}")?,
        }
        if let Some(variable) = variable {
            write!(f, " (set by {variable})")?;
        }
        write!(f, "{}", Suggestion(suggestion.as_deref()))
    }
}

/// The `; did you mean` tail of a refusal, empty without a suggestion.
struct Suggestion<'a>(Option<&'a str>);

impl fmt::Display for Suggestion<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(suggestion) => write!(f, "; did you mean `{suggestion}`?"),
            None => Ok(()),
        }
    }
}

/// The ` at line L, column C` part of a syntax refusal, empty without one.
struct Position(Option<(usize, usize)>);

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some((line, column)) => write!(f, " at line {line}, column {column}"),
            None => Ok(()),
        }
    }
}

/// Returns `message` with every double-quoted run replaced by the redaction
/// placeholder.
///
/// serde quotes a refused value in double quotes and names a field in
/// backticks, so the field names stay and the values go.
pub(crate) fn mask_values(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut quoted = false;
    for character in message.chars() {
        if character == '"' {
            if quoted {
                out.push_str(ferrohealth_secret::REDACTED);
                out.push('"');
            } else {
                out.push('"');
            }
            quoted = !quoted;
        } else if !quoted {
            out.push(character);
        }
    }
    if quoted {
        out.push_str(ferrohealth_secret::REDACTED);
    }
    out
}

/// Returns the 1-based line and column of byte offset `at` in `text`.
pub(crate) fn position(text: &str, at: usize) -> Option<(usize, usize)> {
    let before = text.get(..at)?;
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    Some((line, column))
}

#[cfg(test)]
mod tests {
    use super::{mask_values, position};

    #[test]
    fn quoted_values_are_masked_and_field_names_stay() {
        assert_eq!(
            "invalid type: string \"***\", expected u16",
            mask_values("invalid type: string \"Qz7sentinel\", expected u16")
        );
        assert_eq!(
            "unknown field `pasword`, expected `user` or `password`",
            mask_values("unknown field `pasword`, expected `user` or `password`")
        );
        assert_eq!("an open \"***", mask_values("an open \"Qz7sentinel"));
    }

    #[test]
    fn a_position_is_one_based_and_counts_characters() {
        assert_eq!(Some((1, 1)), position("a = 1", 0));
        assert_eq!(Some((2, 3)), position("a = 1\nbé = 2", 9));
        assert_eq!(None, position("a", 9));
    }
}
