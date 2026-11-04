// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The variables a loader reads from the process environment.

use std::collections::BTreeMap;

use super::Loader;
use crate::error::ConfigError;

impl Loader {
    /// Collects the variables this loader reads from the process
    /// environment: those under the prefix and the aliases.
    pub(super) fn environment(
        &self,
        variables: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> Result<BTreeMap<String, String>, ConfigError> {
        let reserved = format!("{}_", self.prefix);
        let mut environment = BTreeMap::new();
        for (name, value) in variables {
            // NOTE: no specification governs this: our own design. A name that is
            // not UTF-8 cannot be one of this loader's, so it is legitimately skipped.
            let Some(name) = name.to_str() else {
                continue;
            };
            let read = name.starts_with(&reserved)
                || self
                    .aliases
                    .iter()
                    .any(|alias| alias.reads.iter().any(|read| read == name));
            if !read {
                continue;
            }
            // NOTE: no specification governs this: our own design. The error is the
            // value itself, which may be a secret, so the refusal names only the variable.
            let value = value
                .into_string()
                .map_err(|_value| ConfigError::NotUnicode {
                    name: name.to_owned(),
                })?;
            environment.insert(name.to_owned(), value);
        }
        Ok(environment)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::Loader;
    use crate::error::ConfigError;

    fn loader() -> Loader {
        Loader::new("FERROSYS")
            .expect("the prefix is valid")
            .alias("RUST_LOG", "telemetry.filter")
            .alias("FERROSYS_LOG_FORMAT", "telemetry.format")
    }

    #[test]
    fn the_process_environment_keeps_only_the_variables_the_loader_reads() {
        let variables = vec![
            (
                OsString::from("FERROSYS__SERVER__LISTEN"),
                OsString::from("0.0.0.0:1"),
            ),
            (OsString::from("RUST_LOG"), OsString::from("debug")),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
        ];
        let environment = loader()
            .environment(variables.into_iter())
            .expect("UTF-8 values");
        assert_eq!(
            vec!["FERROSYS__SERVER__LISTEN", "RUST_LOG"],
            environment.keys().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_the_loader_reads_must_be_utf8() {
        use std::os::unix::ffi::OsStringExt as _;
        let variables = vec![(
            OsString::from("FERROSYS__SERVER__LISTEN"),
            OsString::from_vec(vec![0xff, 0xfe]),
        )];
        let error = loader()
            .environment(variables.into_iter())
            .expect_err("bytes that are no UTF-8 are refused");
        assert!(matches!(error, ConfigError::NotUnicode { .. }), "{error:?}");
        let ignored = vec![(OsString::from("OTHER"), OsString::from_vec(vec![0xff]))];
        assert!(
            loader()
                .environment(ignored.into_iter())
                .expect("a foreign variable is skipped")
                .is_empty()
        );
    }
}
