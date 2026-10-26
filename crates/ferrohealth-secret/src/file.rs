// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The key-or-file rule for secrets.
//!
//! Every secret of a FerroHEALTH configuration can be given inline, under its
//! key, or as a file named by its `<key>_file` sibling, so a container
//! platform can mount it (a Docker or Kubernetes secret). Exactly one of the
//! two may be set. The file's content is the secret with its trailing line
//! ending removed, since an editor or `echo` adds one; any other whitespace
//! is part of the secret. An empty secret is refused. The text read is
//! zeroed once the secret is taken from it.

use std::path::{Path, PathBuf};

use secrecy::SecretString;
use secrecy::zeroize::Zeroizing;

/// Why a secret could not be read.
///
/// Every variant names the configuration key, and the file variants the
/// path; none carries the secret.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// Both the key and its `_file` sibling are set.
    #[error("{key} and {key}_file are both set; set exactly one")]
    Both {
        /// The key of the secret.
        key: String,
    },
    /// Neither the key nor its `_file` sibling is set, for a secret that is
    /// required.
    #[error("{key} is not set; set {key} or {key}_file")]
    Missing {
        /// The key of the secret.
        key: String,
    },
    /// The file named by `<key>_file` could not be read.
    #[error("{key}_file names {}, which could not be read", path.display())]
    Read {
        /// The key of the secret.
        key: String,
        /// The file.
        path: PathBuf,
        /// What the file system reported.
        #[source]
        source: std::io::Error,
    },
    /// The file named by `<key>_file` holds no secret.
    #[error("{key}_file names {}, which is empty", path.display())]
    Empty {
        /// The key of the secret.
        key: String,
        /// The file.
        path: PathBuf,
    },
}

/// Returns the secret `key` names, inline or from the file its `_file`
/// sibling names, as the redacting type `T` that holds it, or `None` when
/// neither is set.
///
/// `T` is any type built from a [`SecretString`], such as
/// [`crate::secret::Secret`] or [`crate::secret_url::SecretUrl`].
///
/// # Errors
///
/// Returns [`SecretError::Both`] when both are set, and the errors of
/// [`read_secret_file`] when the file cannot be read or is empty.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::file::read_secret;
/// use ferrohealth_secret::secret::Secret;
///
/// let inline = Some(Secret::new("Qz7-inline"));
/// let secret = read_secret("cdr.password", inline, None)?;
/// assert_eq!(secret.map(|s| s.expose().to_owned()).as_deref(), Some("Qz7-inline"));
///
/// let none: Option<Secret> = read_secret("cdr.password", None, None)?;
/// assert!(none.is_none());
/// # Ok::<(), ferrohealth_secret::file::SecretError>(())
/// ```
pub fn read_secret<T>(
    key: &str,
    inline: Option<T>,
    file: Option<&Path>,
) -> Result<Option<T>, SecretError>
where
    T: From<SecretString>,
{
    match (inline, file) {
        (Some(_), Some(_)) => Err(SecretError::Both {
            key: key.to_owned(),
        }),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(path)) => read_secret_file(key, path).map(|value| Some(T::from(value))),
        (None, None) => Ok(None),
    }
}

/// Returns the secret `key` names, inline or from the file its `_file`
/// sibling names, for a secret that must be set.
///
/// # Errors
///
/// Returns [`SecretError::Missing`] when neither is set, and the errors of
/// [`read_secret`] otherwise.
pub fn require_secret<T>(
    key: &str,
    inline: Option<T>,
    file: Option<&Path>,
) -> Result<T, SecretError>
where
    T: From<SecretString>,
{
    read_secret(key, inline, file)?.ok_or_else(|| SecretError::Missing {
        key: key.to_owned(),
    })
}

/// Reads the secret file `path`, the value of `<key>_file`, into a
/// [`SecretString`].
///
/// The trailing line ending (`\n` or `\r\n`, repeated) is removed, and the
/// text read is zeroed once the secret is taken from it.
///
/// # Errors
///
/// Returns [`SecretError::Read`] when the file cannot be read as UTF-8 text,
/// and [`SecretError::Empty`] when nothing is left.
pub fn read_secret_file(key: &str, path: &Path) -> Result<SecretString, SecretError> {
    let text = std::fs::read_to_string(path)
        .map(Zeroizing::new)
        .map_err(|source| SecretError::Read {
            key: key.to_owned(),
            path: path.to_path_buf(),
            source,
        })?;
    let value = text.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err(SecretError::Empty {
            key: key.to_owned(),
            path: path.to_path_buf(),
        });
    }
    Ok(SecretString::from(value))
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::io::Write as _;
    use std::path::Path;

    use super::{SecretError, read_secret, read_secret_file, require_secret};
    use crate::secret::Secret;
    use crate::secret_url::SecretUrl;

    const SENTINEL: &str = "Qz7sentinel";

    fn file_with(content: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("a temporary file");
        file.write_all(content).expect("the content is written");
        file
    }

    #[test]
    fn the_inline_value_is_taken() {
        let secret = read_secret("cdr.password", Some(Secret::new(SENTINEL)), None)
            .expect("an inline secret reads")
            .expect("the secret is set");
        assert_eq!(SENTINEL, secret.expose());
    }

    #[test]
    fn the_file_value_is_taken_without_its_line_ending() {
        for content in [
            &b"Qz7sentinel"[..],
            b"Qz7sentinel\n",
            b"Qz7sentinel\r\n",
            b"Qz7sentinel\n\n",
        ] {
            let file = file_with(content);
            let secret: Secret = require_secret("cdr.password", None, Some(file.path()))
                .expect("a file secret reads");
            assert_eq!(SENTINEL, secret.expose(), "{content:?}");
        }
    }

    #[test]
    fn other_whitespace_is_part_of_the_secret() {
        let file = file_with(b" Qz7 sentinel \n");
        let secret = read_secret_file("cdr.password", file.path()).expect("the file reads");
        assert_eq!(
            " Qz7 sentinel ",
            secrecy::ExposeSecret::expose_secret(&secret)
        );
    }

    #[test]
    fn a_file_reads_into_any_redacting_type() {
        let file = file_with(b"postgres://u:Qz7sentinel@db.example.org/ferrosys\n");
        let url: SecretUrl =
            require_secret("db.url", None, Some(file.path())).expect("the file reads");
        assert_eq!("postgres://***@db.example.org/ferrosys", url.to_string());
    }

    #[test]
    fn both_set_is_refused_naming_the_key() {
        let file = file_with(SENTINEL.as_bytes());
        let error = read_secret(
            "cdr.password",
            Some(Secret::new(SENTINEL)),
            Some(file.path()),
        )
        .expect_err("both set is refused");
        assert!(
            matches!(&error, SecretError::Both { key } if key == "cdr.password"),
            "{error:?}"
        );
        let shown = error.to_string();
        assert_eq!(
            "cdr.password and cdr.password_file are both set; set exactly one",
            shown
        );
        assert!(
            !shown.contains(SENTINEL),
            "a rendering shows what it must hide"
        );
    }

    #[test]
    fn neither_set_is_none_or_missing() {
        let none: Option<Secret> =
            read_secret("cdr.password", None, None).expect("neither set reads");
        assert!(none.is_none(), "neither set is no secret");
        let error = require_secret::<Secret>("cdr.password", None, None)
            .expect_err("a required secret is refused");
        assert!(
            matches!(&error, SecretError::Missing { key } if key == "cdr.password"),
            "{error:?}"
        );
        assert_eq!(
            "cdr.password is not set; set cdr.password or cdr.password_file",
            error.to_string()
        );
    }

    #[test]
    fn a_missing_file_is_refused_with_its_cause() {
        let path = Path::new("/nonexistent/ferrosys/secret");
        let error = read_secret_file("cdr.password", path).expect_err("a missing file is refused");
        assert!(
            matches!(&error, SecretError::Read { key, path: p, .. } if key == "cdr.password" && p == path),
            "{error:?}"
        );
        let source = error.source().expect("the cause is carried");
        let io = source
            .downcast_ref::<std::io::Error>()
            .expect("the cause is the I/O error");
        assert_eq!(std::io::ErrorKind::NotFound, io.kind());
        assert!(
            error
                .to_string()
                .starts_with("cdr.password_file names /nonexistent/ferrosys/secret"),
            "{error}"
        );
    }

    #[test]
    fn a_file_that_is_not_text_is_refused() {
        let file = file_with(&[0xff, 0xfe, 0x00]);
        let error = read_secret_file("cdr.password", file.path())
            .expect_err("bytes that are no UTF-8 are refused");
        assert!(matches!(error, SecretError::Read { .. }), "{error:?}");
    }

    #[test]
    fn an_empty_file_is_refused() {
        for content in [&b""[..], b"\n", b"\r\n"] {
            let file = file_with(content);
            let error = read_secret_file("cdr.password", file.path())
                .expect_err("an empty file is refused");
            assert!(
                matches!(&error, SecretError::Empty { key, .. } if key == "cdr.password"),
                "{error:?}"
            );
        }
    }
}
