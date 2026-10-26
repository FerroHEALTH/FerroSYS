// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The environment overrides, applied over the file before the tree is read.
//!
//! An environment variable carries text, and the tree decides what the text
//! is. Every override is first placed as the string it is, so a string key
//! takes `12345`, `true` or `1.5` verbatim. When the tree then refuses a
//! string where it wants a number, a boolean or an array, and the text reads
//! as that TOML value, the override is placed typed and the tree is read
//! again. Where the tree wants a sequence and the text is no TOML array, it
//! is placed as the comma-separated list of its parts.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::error::ConfigError;

/// One variable that sets one key.
#[derive(Debug)]
pub(crate) struct Override {
    /// The variable name, for a refusal and the provenance.
    name: String,
    /// The lower-case key segments, the section first.
    path: Vec<String>,
    /// The text the variable carries.
    raw: String,
    /// The text read as a TOML value that is not a string, when it reads so.
    typed: Option<toml::Value>,
}

/// How an override's text is placed in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// As the string it is.
    Text,
    /// As the TOML value it reads as.
    Typed,
    /// As the comma-separated list of its parts, each trimmed.
    List,
}

impl Override {
    /// Returns the override of the variable `name` that sets the dotted key
    /// `key` to `raw`.
    pub(crate) fn new(name: &str, key: &str, raw: &str) -> Self {
        Self {
            name: name.to_owned(),
            path: key.split('.').map(str::to_owned).collect(),
            raw: raw.to_owned(),
            typed: typed_value(raw),
        }
    }

    /// Returns the override `name` makes under `prefix` (the prefix followed
    /// by its `__` separator), or `None` for a variable outside it.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::OverrideName`] for a name that addresses no
    /// section and key.
    pub(crate) fn parse(prefix: &str, name: &str, raw: &str) -> Result<Option<Self>, ConfigError> {
        let separator = format!("{prefix}__");
        let Some(rest) = name.strip_prefix(&separator) else {
            return Ok(None);
        };
        let path: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
        if path.iter().any(String::is_empty) {
            return Err(ConfigError::OverrideName {
                name: name.to_owned(),
                prefix: prefix.to_owned(),
            });
        }
        Ok(Some(Self {
            name: name.to_owned(),
            path,
            raw: raw.to_owned(),
            typed: typed_value(raw),
        }))
    }

    /// Returns the dotted key the override sets.
    pub(crate) fn key(&self) -> String {
        self.path.join(".")
    }

    /// Returns the variable name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Places the override onto `table` as `placement` says.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::OverrideShape`] when a parent segment holds a
    /// value that is not a section.
    pub(crate) fn apply(
        &self,
        table: &mut toml::Table,
        placement: Placement,
    ) -> Result<(), ConfigError> {
        let Some((key, parents)) = self.path.split_last() else {
            return Err(ConfigError::OverrideName {
                name: self.name.clone(),
                prefix: String::new(),
            });
        };
        let mut cursor = table;
        let mut walked = Vec::new();
        for parent in parents {
            walked.push(parent.as_str());
            let entry = cursor
                .entry(parent.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            let toml::Value::Table(next) = entry else {
                return Err(ConfigError::OverrideShape {
                    name: self.name.clone(),
                    key: walked.join("."),
                });
            };
            cursor = next;
        }
        let value = match (placement, &self.typed) {
            (Placement::Typed, Some(value)) => value.clone(),
            (Placement::List, _) => toml::Value::Array(
                self.raw
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| toml::Value::String(part.to_owned()))
                    .collect(),
            ),
            _ => toml::Value::String(self.raw.clone()),
        };
        cursor.insert(key.clone(), value);
        Ok(())
    }
}

/// Returns the override the tree refused as a string, and how to place it
/// on the next read: typed when its text reads as another TOML value, or as
/// a comma-separated list when the tree wants a sequence.
///
/// `merged` is the text the tree was read from, `refused` the span its
/// refusal names, `wants_sequence` whether the refusal asked for one, and
/// `placements` how each override is placed so far.
pub(crate) fn retype(
    overrides: &[Override],
    placements: &BTreeMap<usize, Placement>,
    merged: &str,
    refused: &Range<usize>,
    wants_sequence: bool,
) -> Option<(usize, Placement)> {
    // NOTE: no specification governs this: our own design. A merged text that
    // does not parse leaves the refusal standing as the reader reported it.
    let document = toml::de::DeTable::parse(merged).ok()?.into_inner();
    overrides.iter().enumerate().find_map(|(index, item)| {
        let span = value_span(&document, &item.path)?;
        if !(span.start <= refused.start && refused.end <= span.end) {
            return None;
        }
        if placements.get(&index).copied().unwrap_or(Placement::Text) != Placement::Text {
            return None;
        }
        match (&item.typed, wants_sequence) {
            (Some(toml::Value::Array(_)), true) | (Some(_), false) => {
                Some((index, Placement::Typed))
            }
            (_, true) => Some((index, Placement::List)),
            (None, false) => None,
        }
    })
}

/// Returns the span of the value `path` addresses in `document`.
fn value_span(document: &toml::de::DeTable<'_>, path: &[String]) -> Option<Range<usize>> {
    let (first, rest) = path.split_first()?;
    let mut value = document.get(first.as_str())?;
    for segment in rest {
        value = value.get_ref().get(segment.as_str())?;
    }
    Some(value.span())
}

/// Reads `raw` as a TOML value that is not a string: a number, a boolean, a
/// date-time, an array or an inline table.
fn typed_value(raw: &str) -> Option<toml::Value> {
    // NOTE: no specification governs this: our own design. A parse failure IS
    // the answer here, because text that is not TOML syntax stays a string.
    let parsed = toml::from_str::<toml::Table>(&format!("value = {raw}"))
        .ok()
        .and_then(|table| table.get("value").cloned())?;
    (!parsed.is_str()).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::{Override, Placement, typed_value};
    use crate::error::ConfigError;

    #[test]
    fn an_environment_value_reads_typed_only_as_toml_syntax_that_is_not_a_string() {
        assert_eq!(Some(toml::Value::Integer(5)), typed_value("5"));
        assert_eq!(Some(toml::Value::Boolean(true)), typed_value("true"));
        assert_eq!(Some(toml::Value::Float(1.5)), typed_value("1.5"));
        assert_eq!(None, typed_value("\"quoted\""));
        assert_eq!(None, typed_value("http://cdr.invalid/v1"));
        assert_eq!(None, typed_value("0.0.0.0:8080"));
    }

    #[test]
    fn an_override_maps_its_name_to_a_lower_case_key() {
        let item = Override::parse("FERROSYS", "FERROSYS__SERVER__BODY_LIMIT_BYTES", "10")
            .expect("the name parses")
            .expect("the name is under the prefix");
        assert_eq!("server.body_limit_bytes", item.key());
        assert_eq!("FERROSYS__SERVER__BODY_LIMIT_BYTES", item.name());
        assert!(
            Override::parse("FERROSYS", "OTHER__SERVER__LISTEN", "x")
                .expect("a foreign name is no error")
                .is_none(),
            "a variable outside the prefix is no override"
        );
    }

    #[test]
    fn an_override_name_with_an_empty_segment_is_refused() {
        assert_eq!(
            "spec_profile",
            Override::parse("FERROSYS", "FERROSYS__SPEC_PROFILE", "x")
                .expect("a top-level key parses")
                .expect("the name is under the prefix")
                .key()
        );
        for name in ["FERROSYS__", "FERROSYS__SERVER__", "FERROSYS____LISTEN"] {
            let error =
                Override::parse("FERROSYS", name, "x").expect_err("every segment names something");
            assert!(
                matches!(error, ConfigError::OverrideName { .. }),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_override_under_a_value_is_refused_naming_the_value() {
        let mut table: toml::Table =
            toml::from_str("[server]\nlisten = \"127.0.0.1:1\"\n").expect("toml");
        let item = Override::new("FERROSYS__SERVER__LISTEN__PORT", "server.listen.port", "1");
        let error = item
            .apply(&mut table, Placement::Text)
            .expect_err("listen is a value");
        assert!(
            matches!(&error, ConfigError::OverrideShape { key, .. } if key == "server.listen"),
            "{error:?}"
        );
    }

    #[test]
    fn a_list_placement_splits_on_commas_and_trims() {
        let mut table = toml::Table::new();
        Override::new("FERROSYS__AUTH__AUDIENCE", "auth.audience", " a, b ,,c ")
            .apply(&mut table, Placement::List)
            .expect("the list is placed");
        let audience = table
            .get("auth")
            .and_then(|auth| auth.get("audience"))
            .cloned();
        assert_eq!(
            Some(toml::Value::Array(vec!["a".into(), "b".into(), "c".into()])),
            audience
        );
    }
}
