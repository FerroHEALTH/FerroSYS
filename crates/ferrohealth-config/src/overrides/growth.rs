// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The elements the overrides added to an array of tables past its end,
//! and the gaps they leave.

use std::collections::BTreeMap;

use crate::error::ConfigError;

/// The elements one application of the overrides padded an array with, to
/// reach an index past its end, and that no override has set since.
///
/// The overrides apply in their layer order, which is no index order
/// (`__10__` sorts before `__2__`), so a gap is judged once every override
/// is placed.
#[derive(Debug, Default)]
pub(crate) struct Growth {
    /// The padded elements, by the dotted key of their array and their
    /// index, with the override that padded them.
    padded: BTreeMap<(String, usize), String>,
}

impl Growth {
    /// Makes `elements`, the array at the dotted key `key`, hold the element
    /// `index`, which the override `name` sets: an element past the end
    /// is added, every one between padded as an empty table.
    pub(super) fn reach(
        &mut self,
        key: &str,
        elements: &mut Vec<toml::Value>,
        index: usize,
        name: &str,
    ) {
        while elements.len() < index {
            self.padded
                .insert((key.to_owned(), elements.len()), name.to_owned());
            elements.push(toml::Value::Table(toml::Table::new()));
        }
        if elements.len() == index {
            elements.push(toml::Value::Table(toml::Table::new()));
        }
        let _set: Option<String> = self.padded.remove(&(key.to_owned(), index));
    }

    /// Forgets every padded element at or below the dotted key `key`, whose
    /// value an override replaced whole.
    pub(super) fn replaced(&mut self, key: &str) {
        self.padded.retain(|(array, _), _| {
            !(array == key
                || array
                    .strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with(['.', '['])))
        });
    }

    /// Refuses the first element an override skipped.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::OverrideGap`] naming the array, the first
    /// index no source set, and the override that set an element past it.
    pub(crate) fn check(self) -> Result<(), ConfigError> {
        match self.padded.into_iter().next() {
            None => Ok(()),
            Some(((key, missing), name)) => Err(ConfigError::OverrideGap { name, key, missing }),
        }
    }
}
