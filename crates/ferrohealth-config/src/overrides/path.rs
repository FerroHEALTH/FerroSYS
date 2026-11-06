// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The path an override sets: keys, and the element indices of the arrays
//! of tables between them.

use crate::error::ConfigError;

/// One step of an override's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment {
    /// A key of a table, lower-case for a variable.
    Key(String),
    /// An element of the array of tables the key before it names.
    Index(usize),
}

/// Returns the path of the variable `name` from the segments after its
/// prefix, `rest`: `AUTH__ISSUER__0__URL` is `auth`, `issuer`, `0`, `url`.
///
/// # Errors
///
/// Returns [`ConfigError::OverrideName`] for an empty segment and
/// [`ConfigError::OverrideIndex`] for an index that is ambiguous or stands
/// where no array of tables is named.
pub(super) fn of_variable(
    name: &str,
    prefix: &str,
    rest: &str,
) -> Result<Vec<Segment>, ConfigError> {
    let mut path = Vec::new();
    for part in rest.split("__") {
        if part.is_empty() {
            return Err(ConfigError::OverrideName {
                name: name.to_owned(),
                prefix: prefix.to_owned(),
            });
        }
        path.push(segment(name, &part.to_ascii_lowercase())?);
    }
    checked(name, path)
}

/// Returns the path of the dotted key `key` of a `--set` argument or an
/// alias, where an element index is a segment of its own (`issuer.0.url`)
/// or follows its key in brackets (`issuer[0].url`).
///
/// # Errors
///
/// Returns [`ConfigError::OverrideIndex`] for an index that is ambiguous or
/// stands where no array of tables is named; `name` names the source.
pub(super) fn of_key(name: &str, key: &str) -> Result<Vec<Segment>, ConfigError> {
    let mut path = Vec::new();
    for part in key.split('.') {
        match part.strip_suffix(']').and_then(|head| head.split_once('[')) {
            Some((key, index)) if !key.is_empty() => {
                path.push(segment(name, key)?);
                path.push(index_of(name, index)?);
            }
            _ => path.push(segment(name, part)?),
        }
    }
    checked(name, path)
}

/// Renders `path` as a dotted key, every element index in brackets after its
/// key: `auth.issuer[0].url`, the form a refusal names a key in.
pub(crate) fn render(path: &[Segment]) -> String {
    let mut key = String::new();
    for segment in path {
        match segment {
            Segment::Key(name) => {
                if !key.is_empty() {
                    key.push('.');
                }
                key.push_str(name);
            }
            Segment::Index(index) => {
                key.push('[');
                key.push_str(&index.to_string());
                key.push(']');
            }
        }
    }
    key
}

/// Reads `part` as an element index when it is all digits, else as a key.
fn segment(name: &str, part: &str) -> Result<Segment, ConfigError> {
    if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) {
        index_of(name, part)
    } else {
        Ok(Segment::Key(part.to_owned()))
    }
}

/// Reads `part` as an element index: a decimal number without a leading
/// zero, so two names never address one element.
fn index_of(name: &str, part: &str) -> Result<Segment, ConfigError> {
    let refused = || ConfigError::OverrideIndex {
        name: name.to_owned(),
        segment: part.to_owned(),
    };
    if part.len() > 1 && part.starts_with('0') {
        return Err(refused());
    }
    // NOTE: no specification governs this: our own design. Text that is no
    // decimal number, or one past `usize`, names no element.
    part.parse().map(Segment::Index).map_err(|_error| refused())
}

/// Refuses an index that opens the path or follows another index: only an
/// array of tables, named by its key, takes an index.
fn checked(name: &str, path: Vec<Segment>) -> Result<Vec<Segment>, ConfigError> {
    let mut previous_is_key = false;
    for segment in &path {
        match segment {
            Segment::Key(_) => previous_is_key = true,
            Segment::Index(index) if !previous_is_key => {
                return Err(ConfigError::OverrideIndex {
                    name: name.to_owned(),
                    segment: index.to_string(),
                });
            }
            Segment::Index(_) => previous_is_key = false,
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{Segment, of_key, of_variable, render};
    use crate::error::ConfigError;

    fn key(name: &str) -> Segment {
        Segment::Key(name.to_owned())
    }

    #[test]
    fn a_digit_segment_is_an_element_index_in_either_form() {
        let path = vec![key("auth"), key("issuer"), Segment::Index(12), key("url")];
        assert_eq!(
            path,
            of_variable("V", "P", "AUTH__ISSUER__12__URL").expect("the name parses")
        );
        assert_eq!(path, of_key("--set", "auth.issuer.12.url").expect("dotted"));
        assert_eq!(
            path,
            of_key("--set", "auth.issuer[12].url").expect("bracketed")
        );
        assert_eq!("auth.issuer[12].url", render(&path));
        assert_eq!(
            vec![key("server"), key("listen")],
            of_key("--set", "server.listen").expect("no index")
        );
    }

    #[test]
    fn an_ambiguous_or_misplaced_index_is_refused() {
        for rest in ["AUTH__ISSUER__01__URL", "0__URL", "AUTH__ISSUER__0__1"] {
            let error = of_variable("V", "P", rest).expect_err("refused");
            assert!(
                matches!(error, ConfigError::OverrideIndex { .. }),
                "{rest}: {error:?}"
            );
        }
        for dotted in [
            "auth.issuer[00].url",
            "auth.issuer[x].url",
            "auth.issuer.1[0]",
        ] {
            let error = of_key("--set", dotted).expect_err("refused");
            assert!(
                matches!(error, ConfigError::OverrideIndex { .. }),
                "{dotted}: {error:?}"
            );
        }
    }
}
