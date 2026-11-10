// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The merged TOML table the loader builds: finding the key at a position
//! of its text, and reading, taking, removing and filling a dotted path.

use std::ops::Range;

use crate::provenance::indexed;

/// Returns the dotted key whose key or value holds the start of `span` in
/// the TOML text `merged`, the deepest one first.
pub(super) fn key_at(merged: &str, span: &Range<usize>) -> Option<String> {
    let document = toml::de::DeTable::parse(merged).ok()?.into_inner();
    find_key(&document, span.start).map(|path| path.join("."))
}

/// Walks `table` for the deepest key that holds `at`, a table in an array
/// named with its index (`manager[1]`).
pub(super) fn find_key(table: &toml::de::DeTable<'_>, at: usize) -> Option<Vec<String>> {
    for (key, value) in table {
        let name = key.get_ref().to_string();
        if let Some(inner) = value.get_ref().as_table()
            && let Some(mut below) = find_key(inner, at)
        {
            below.insert(0, name);
            return Some(below);
        }
        // NOTE: no specification governs this: our own design; an element of an
        // array of tables is named by its index, as `pixm.manager[0].method`.
        if let Some(elements) = value.get_ref().as_array() {
            for (index, element) in elements.iter().enumerate() {
                let Some(inner) = element.get_ref().as_table() else {
                    continue;
                };
                if let Some(mut below) = find_key(inner, at) {
                    below.insert(0, format!("{name}[{index}]"));
                    return Some(below);
                }
                if element.span().contains(&at) {
                    return Some(vec![format!("{name}[{index}]")]);
                }
            }
        }
        if key.span().contains(&at) || value.span().contains(&at) {
            return Some(vec![name]);
        }
    }
    None
}

/// The segments of the dotted key `key`, an indexed segment such as
/// `manager[1]` kept whole.
pub(super) fn segments(key: &str) -> Vec<String> {
    key.split('.').map(str::to_owned).collect()
}

/// The path dropping the refused key `key` removes: its [`segments`], with a
/// last indexed segment naming its array, so dropping it drops every element
/// and no index moves.
pub(super) fn dropped_path(key: &str) -> Vec<String> {
    let mut path = segments(key);
    if let Some(last) = path.last_mut() {
        let (name, _) = indexed(last);
        *last = name.to_owned();
    }
    path
}

/// Removes the value at `path` in `table`, when there is one.
pub(super) fn remove(table: &mut toml::Table, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut cursor = table;
    for parent in parents {
        let (name, index) = indexed(parent);
        let value = cursor.get_mut(name);
        let next = match (value, index) {
            (Some(toml::Value::Table(inner)), None) => Some(inner),
            (Some(toml::Value::Array(elements)), Some(index)) => {
                elements.get_mut(index).and_then(toml::Value::as_table_mut)
            }
            _ => None,
        };
        // NOTE: no specification governs this: our own design; a refused key that is
        // already gone has nothing left to remove.
        let Some(next) = next else {
            return;
        };
        cursor = next;
    }
    let _removed: Option<toml::Value> = cursor.remove(last.as_str());
}

/// Puts every key of `above` into `below`, merging the tables both hold.
pub(super) fn overlay(below: &mut toml::Table, above: toml::Table) {
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
///
/// A secret and its `<key>_file` sibling are one key: a default of either
/// is left out where `table` holds the other, so a default never sets the
/// second half of a pair the sources already set.
pub(super) fn fill(table: &mut toml::Table, defaults: &toml::Table) {
    for (key, default) in defaults {
        if !table.contains_key(key) && sibling_is_set(table, key) {
            continue;
        }
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

/// Whether `table` holds the other half of the secret pair `key` names:
/// `<key>_file` for a secret, the secret for a `<key>_file`.
fn sibling_is_set(table: &toml::Table, key: &str) -> bool {
    let other = key
        .strip_suffix("_file")
        .map_or_else(|| format!("{key}_file"), str::to_owned);
    table.contains_key(&other)
}

/// Removes every value of `table` that is a secret's redaction marker, and
/// returns the dotted key of the first value that holds the marker inside
/// other text, such as a URL with its userinfo masked.
///
/// A secret serializes as [`ferrohealth_secret::REDACTED`], so a default
/// read from a serialized tree holds the marker where the secret stood: the
/// key is dropped and the tree's `Default` stands for it. A value that
/// holds the marker among other text cannot be told from a masked one.
pub(super) fn strip_redacted(table: &mut toml::Table) -> Option<String> {
    let marker = ferrohealth_secret::REDACTED;
    table.retain(|_, value| value.as_str() != Some(marker));
    for (key, value) in table.iter_mut() {
        let masked = match value {
            toml::Value::Table(inner) => {
                strip_redacted(inner).map(|below| format!("{key}.{below}"))
            }
            toml::Value::String(text) => text.contains(marker).then(|| key.clone()),
            toml::Value::Array(elements) => elements
                .iter()
                .any(|element| holds_marker(element, marker))
                .then(|| key.clone()),
            _ => None,
        };
        if masked.is_some() {
            return masked;
        }
    }
    None
}

/// Whether `value` or anything inside it holds `marker`.
fn holds_marker(value: &toml::Value, marker: &str) -> bool {
    match value {
        toml::Value::String(text) => text.contains(marker),
        toml::Value::Array(elements) => {
            elements.iter().any(|element| holds_marker(element, marker))
        }
        toml::Value::Table(inner) => inner.values().any(|element| holds_marker(element, marker)),
        _ => false,
    }
}

/// Returns the dotted key above `key`, or `None` for a top-level key.
pub(super) fn parent(key: &str) -> Option<&str> {
    key.rsplit_once('.').map(|(parent, _)| parent)
}
