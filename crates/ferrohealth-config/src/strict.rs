// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The strict checks: the sweep of the reserved environment namespace and
//! the did-you-mean suggestion every refusal of an unknown name carries.

use std::collections::BTreeMap;

use crate::error::ConfigError;

/// Sweeps every variable that starts with `<prefix>_`.
///
/// A variable of the uniform form `<prefix>__SECTION__KEY` is an override
/// and is checked when the tree is read; one of `known` (an alias or a name
/// the product reserved, such as `<prefix>_CONFIG`) passes. Every other
/// variable under the prefix is refused, with the spelling it probably
/// meant: the uniform form when its first word is followed by `__`, else the
/// closest name of `known`. The refusals come back sorted by name.
#[must_use]
pub fn sweep(
    prefix: &str,
    environment: &BTreeMap<String, String>,
    known: &[&str],
) -> Vec<ConfigError> {
    let reserved = format!("{prefix}_");
    let uniform = format!("{prefix}__");
    environment
        .keys()
        .filter(|name| name.starts_with(&reserved) && !name.starts_with(&uniform))
        .filter(|name| !known.contains(&name.as_str()))
        .map(|name| {
            let tail = name.get(reserved.len()..).unwrap_or_default();
            let suggestion = if tail.contains("__") {
                Some(format!("{uniform}{tail}"))
            } else {
                did_you_mean(name, known)
            };
            ConfigError::UnknownVariable {
                name: name.clone(),
                suggestion,
            }
        })
        .collect()
}

/// Returns the candidate closest to `unknown` within an edit distance of
/// two, ties broken by the order of the names, or `None` when none is that
/// close. The comparison ignores ASCII case.
#[must_use]
pub fn did_you_mean(unknown: &str, candidates: &[&str]) -> Option<String> {
    let unknown = unknown.to_ascii_lowercase();
    candidates
        .iter()
        .map(|candidate| {
            (
                *candidate,
                distance(&unknown, &candidate.to_ascii_lowercase()),
            )
        })
        .filter(|(_, distance)| *distance <= 2)
        .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)))
        .map(|(candidate, _)| candidate.to_owned())
}

/// The optimal string alignment distance (restricted Damerau-Levenshtein):
/// one insertion, deletion, substitution or swap of two adjacent characters
/// costs one.
#[expect(
    clippy::indexing_slicing,
    reason = "every index is bounded by the loop headers against vectors sized from the same \
              bounds: i runs 1..=n over a (length n) and the rows (length m + 1), j runs 1..=m \
              over b (length m), and the i - 2 and j - 2 reads sit behind the i > 1 && j > 1 test"
)]
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut before = vec![0_usize; m + 1];
    let mut previous: Vec<usize> = (0..=m).collect();
    let mut current = vec![0_usize; m + 1];
    for i in 1..=n {
        current[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut value = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                value = value.min(before[j - 2] + 1);
            }
            current[j] = value;
        }
        std::mem::swap(&mut before, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[m]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{did_you_mean, distance, sweep};
    use crate::error::ConfigError;

    #[test]
    fn the_distance_counts_edits_and_adjacent_swaps() {
        assert_eq!(0, distance("listen", "listen"));
        assert_eq!(1, distance("listn", "listen"));
        assert_eq!(1, distance("lsiten", "listen"));
        assert_eq!(9, distance("telemetry", "db"));
        assert_eq!(2, distance("", "db"));
        assert_eq!(1, distance("zürich", "zurich"));
    }

    #[test]
    fn a_suggestion_is_the_closest_name_within_two_edits() {
        let keys = ["listen", "body_limit_bytes", "request_timeout_ms"];
        assert_eq!(Some("listen".to_owned()), did_you_mean("listn", &keys));
        assert_eq!(Some("listen".to_owned()), did_you_mean("LISTEN", &keys));
        assert_eq!(None, did_you_mean("completely_bogus", &keys));
        assert_eq!(Some("ab".to_owned()), did_you_mean("a", &["ac", "ab"]));
    }

    #[test]
    fn the_sweep_refuses_every_unread_variable_under_the_prefix() {
        let environment = BTreeMap::from(
            [
                ("FERROSYS__SERVER__LISTEN", "0.0.0.0:9"),
                ("FERROSYS_CONFIG", "/etc/ferrosys.toml"),
                ("FERROSYS_LOG_FORMAT", "json"),
                ("FERROSYS_SERVER__LISTEN", "0.0.0.0:9"),
                ("FERROSYS_CONFG", "/etc/ferrosys.toml"),
                ("FERROSYS_BOGUS", "1"),
                ("OTHER_VARIABLE", "1"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
        let refused = sweep(
            "FERROSYS",
            &environment,
            &["FERROSYS_CONFIG", "FERROSYS_LOG_FORMAT"],
        );
        let shown: Vec<(String, Option<String>)> = refused
            .into_iter()
            .map(|error| match error {
                ConfigError::UnknownVariable { name, suggestion } => (name, suggestion),
                other => panic!("an unexpected refusal: {other:?}"),
            })
            .collect();
        assert_eq!(
            vec![
                ("FERROSYS_BOGUS".to_owned(), None),
                (
                    "FERROSYS_CONFG".to_owned(),
                    Some("FERROSYS_CONFIG".to_owned())
                ),
                (
                    "FERROSYS_SERVER__LISTEN".to_owned(),
                    Some("FERROSYS__SERVER__LISTEN".to_owned())
                ),
            ],
            shown
        );
    }

    #[test]
    fn a_refused_variable_names_its_suggestion() {
        let error = ConfigError::UnknownVariable {
            name: "FERROSYS_CONFG".to_owned(),
            suggestion: Some("FERROSYS_CONFIG".to_owned()),
        };
        assert_eq!(
            "FERROSYS_CONFG is no configuration variable; did you mean `FERROSYS_CONFIG`?",
            error.to_string()
        );
    }
}
