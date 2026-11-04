// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! How the loader reads one refusal from serde's error, and collects the
//! refusals of one load into one error.

use crate::error::{ConfigError, mask_values};
use crate::provenance::Provenance;
use crate::strict::did_you_mean;

use super::table::{key_at, parent, segments};

/// The refusal of `error`, read from the merged text, and the dotted key it
/// names.
pub(super) fn invalid(
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
pub(super) fn follows_a_drop(
    error: &toml::de::Error,
    key: Option<&str>,
    dropped: &[Vec<String>],
) -> bool {
    let Some(field) = error
        .message()
        .split_once("missing field `")
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(field, _)| field)
    else {
        return false;
    };
    let mut path: Vec<String> = key.map(|key| segments(key, false)).unwrap_or_default();
    path.push(field.to_owned());
    dropped.contains(&path)
}

/// The one error of `errors`, or all of them together.
pub(super) fn collected(mut errors: Vec<ConfigError>) -> ConfigError {
    if errors.len() == 1 {
        errors.remove(0)
    } else {
        ConfigError::Refusals { refusals: errors }
    }
}

/// Returns the closest expected name for serde's refusal of an unknown
/// field or variant (``unknown field `x`, expected one of `a`, `b` ``), and
/// whether it names a field.
pub(super) fn suggest(message: &str) -> Option<(String, bool)> {
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
    use super::suggest;

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
}
