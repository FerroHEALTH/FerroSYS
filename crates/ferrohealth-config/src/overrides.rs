// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The environment overrides, applied over the file before the tree is read.
//!
//! An environment variable carries text, and the tree decides what the text
//! is. Every override is first placed as the string it is, so a string key
//! takes `12345`, `true` or `1.5` verbatim. When the tree then refuses a
//! string where it wants a number, a boolean or an array, and the text reads
//! as that TOML value, the override is placed typed and the tree is read
//! again.

use std::collections::BTreeSet;
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
        if path.len() < 2 || path.iter().any(String::is_empty) {
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

    /// Places the override onto `table`, typed or as its text.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::OverrideShape`] when a parent segment holds a
    /// value that is not a section.
    pub(crate) fn apply(&self, table: &mut toml::Table, typed: bool) -> Result<(), ConfigError> {
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
        let value = match (&self.typed, typed) {
            (Some(value), true) => value.clone(),
            _ => toml::Value::String(self.raw.clone()),
        };
        cursor.insert(key.clone(), value);
        Ok(())
    }
}

/// Returns the index of the override the tree refused as a string and that
/// reads as another TOML value, so it is placed typed on the next read.
///
/// `merged` is the text the tree was read from, `refused` the span its
/// refusal names, and `typed` the overrides already placed typed.
pub(crate) fn retype(
    overrides: &[Override],
    typed: &BTreeSet<usize>,
    merged: &str,
    refused: &Range<usize>,
) -> Option<usize> {
    // NOTE: no specification governs this: our own design. A merged text that
    // does not parse leaves the refusal standing as the reader reported it.
    let document = toml::de::DeTable::parse(merged).ok()?.into_inner();
    overrides.iter().enumerate().find_map(|(index, item)| {
        if typed.contains(&index) || item.typed.is_none() {
            return None;
        }
        let span = value_span(&document, &item.path)?;
        (span.start <= refused.start && refused.end <= span.end).then_some(index)
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
    use super::{Override, typed_value};
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
    fn an_override_name_without_a_section_and_a_key_is_refused() {
        for name in [
            "FERROSYS__LISTEN",
            "FERROSYS__SERVER__",
            "FERROSYS____LISTEN",
        ] {
            let error = Override::parse("FERROSYS", name, "x")
                .expect_err("a section and a key are both required");
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
            .apply(&mut table, false)
            .expect_err("listen is a value");
        assert!(
            matches!(&error, ConfigError::OverrideShape { key, .. } if key == "server.listen"),
            "{error:?}"
        );
    }
}
