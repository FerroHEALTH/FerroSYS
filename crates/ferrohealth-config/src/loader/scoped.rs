// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A product's defaults, read into a table, and the defaults that apply only
//! under a table or under each element of an array of tables a source
//! writes.

use serde::Serialize;

use crate::error::ConfigError;

use super::table::{fill, strip_redacted};

/// Where scoped defaults apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// Under the table, when a source writes it.
    Table,
    /// Under each element of the array of tables a source writes.
    Each,
}

/// Defaults that apply only under what a source writes.
#[derive(Debug, Clone)]
pub(super) struct Scoped {
    path: Vec<String>,
    reach: Reach,
    defaults: toml::Table,
}

impl Scoped {
    /// The defaults `defaults` under the table, or each element of the
    /// array of tables, at the dotted key `at`.
    pub(super) fn new(at: &str, reach: Reach, defaults: toml::Table) -> Self {
        Self {
            path: at.split('.').map(str::to_owned).collect(),
            reach,
            defaults,
        }
    }

    /// Fills the defaults into `tree` under what `sources` writes at the
    /// key: the table, or every element of the array of tables.
    ///
    /// `tree` holds `sources` with defaults filled in, so the element
    /// indices of both agree; only `sources` decides what is written.
    pub(super) fn fill(&self, sources: &toml::Table, tree: &mut toml::Table) {
        if !self.written(sources) {
            return;
        }
        let Some((last, parents)) = self.path.split_last() else {
            return;
        };
        let mut cursor = tree;
        for parent in parents {
            cursor = match cursor.get_mut(parent) {
                Some(toml::Value::Table(inner)) => inner,
                _ => return,
            };
        }
        match (cursor.get_mut(last), self.reach) {
            (Some(toml::Value::Table(table)), Reach::Table) => fill(table, &self.defaults),
            (Some(toml::Value::Array(elements)), Reach::Each) => {
                for element in elements {
                    if let toml::Value::Table(table) = element {
                        fill(table, &self.defaults);
                    }
                }
            }
            // NOTE: no specification governs this: our own design; a value of another
            // shape gets no defaults, and the tree's reader refuses it by its key.
            _ => {}
        }
    }

    /// Whether `sources` holds a value of this reach's shape at the key.
    fn written(&self, sources: &toml::Table) -> bool {
        let Some((last, parents)) = self.path.split_last() else {
            return false;
        };
        let mut cursor = sources;
        for parent in parents {
            cursor = match cursor.get(parent) {
                Some(toml::Value::Table(inner)) => inner,
                _ => return false,
            };
        }
        matches!(
            (cursor.get(last), self.reach),
            (Some(toml::Value::Table(_)), Reach::Table)
                | (Some(toml::Value::Array(_)), Reach::Each)
        )
    }
}

/// Reads `value` as the product's defaults for the dotted key `key`, the
/// whole tree when `key` is empty, nested under the segments of `key`;
/// `named` is the dotted key a refusal names.
///
/// A secret's redaction marker is dropped, so the tree's `Default` stands
/// for the secret.
pub(super) fn serialized<D: Serialize + ?Sized>(
    named: &str,
    key: &str,
    value: &D,
) -> Result<toml::Table, ConfigError> {
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
    if let Some(masked) = strip_redacted(&mut table) {
        let masked = if named.is_empty() {
            masked
        } else {
            format!("{named}.{masked}")
        };
        return Err(ConfigError::Assemble {
            source: <toml::ser::Error as serde::ser::Error>::custom(format!(
                "the product default of `{masked}` holds a masked secret"
            )),
        });
    }
    if !key.is_empty() {
        for segment in key.rsplit('.') {
            table = toml::Table::from_iter([(segment.to_owned(), toml::Value::Table(table))]);
        }
    }
    Ok(table)
}
