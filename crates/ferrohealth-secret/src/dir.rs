// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Named secrets in one directory, read again when the directory changes.
//!
//! A product resolves a secret by a name it learns while it runs, such as a
//! database row that names its HMAC key or its client certificate. Each
//! secret is one file in the directory, named by the secret's name, and an
//! operator adds or rotates one without a restart.
//!
//! - **Kubernetes:** mount one Secret as a volume, without `subPath`. Each
//!   key of the Secret is a file, and the kubelet updates the volume when the
//!   Secret changes, after its sync period and cache delay; a `subPath` mount
//!   and an environment variable never update
//!   (<https://kubernetes.io/docs/concepts/configuration/secret/#using-secrets-as-files-from-a-pod>).
//!   The kubelet writes each update to a new hidden directory and swaps the
//!   `..data` symlink to it in one rename, so this module reads every key
//!   from the directory `..data` names and sees one update whole.
//! - **Compose or a plain directory:** bind-mount the directory. Replace a
//!   file by writing a new file beside it and renaming it over the old one;
//!   a rename is atomic, a write in place is not.
//!
//! [`SecretDir::open`] reads the directory into a [`Snapshot`],
//! [`SecretDir::refresh`] reads it again and swaps the snapshot, and
//! [`SecretDir::watch`] refreshes on a thread at a fixed interval. A reader
//! always answers from one snapshot, so two secrets read from one snapshot
//! come from the same update. A file's content follows the `_file` rule of
//! [`mod@crate::file`]: the trailing line ending is removed and an empty file
//! is refused. No error, `Debug` or `Display` carries a value.

mod scan;

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::REDACTED;

/// A directory of named secrets, shared by every clone.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::dir::SecretDir;
/// use secrecy::ExposeSecret;
///
/// let root = tempfile::tempdir()?;
/// std::fs::write(root.path().join("hmac"), "Qz7-hmac\n")?;
/// let dir = SecretDir::open(root.path())?;
/// assert_eq!(dir.get("hmac")?.expose_secret(), "Qz7-hmac");
/// assert_eq!(dir.names(), ["hmac"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct SecretDir {
    shared: Arc<Shared>,
}

struct Shared {
    root: PathBuf,
    current: RwLock<Arc<Snapshot>>,
    refreshing: Mutex<()>,
}

impl SecretDir {
    /// Opens the directory `root` and reads every secret in it.
    ///
    /// # Errors
    ///
    /// Returns [`SecretDirError::NotADirectory`] when `root` is not a
    /// directory, and the errors of [`SecretDir::refresh`] otherwise.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, SecretDirError> {
        let root = root.into();
        let metadata = std::fs::metadata(&root).map_err(|source| SecretDirError::Unreadable {
            path: root.clone(),
            source,
        })?;
        if !metadata.is_dir() {
            return Err(SecretDirError::NotADirectory { path: root });
        }
        let snapshot = scan::scan(&root)?;
        Ok(Self {
            shared: Arc::new(Shared {
                root,
                current: RwLock::new(Arc::new(snapshot)),
                refreshing: Mutex::new(()),
            }),
        })
    }

    /// The directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    /// The snapshot read last, for a caller that reads more than one secret
    /// from the same update.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        // NOTE: no specification governs this: our own design; a poisoned
        // lock still holds a whole snapshot, since a swap is one assignment.
        Arc::clone(
            &self
                .shared
                .current
                .read()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Returns the secret `name` from the current snapshot.
    ///
    /// # Errors
    ///
    /// The errors of [`Snapshot::get`].
    pub fn get(&self, name: &str) -> Result<SecretString, SecretDirError> {
        self.snapshot().get(name)
    }

    /// Returns the client certificate `name` from the current snapshot.
    ///
    /// # Errors
    ///
    /// The errors of [`Snapshot::get_pair`].
    pub fn get_pair(&self, name: &str) -> Result<ClientCertificate, SecretDirError> {
        self.snapshot().get_pair(name)
    }

    /// The names in the current snapshot, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.snapshot().names().map(str::to_owned).collect()
    }

    /// Reads the directory again and makes what it read the current
    /// snapshot. A failed read leaves the current snapshot in place.
    ///
    /// # Errors
    ///
    /// Returns [`SecretDirError::Unreadable`] when the directory cannot be
    /// read, and [`SecretDirError::Unstable`] when its `..data` symlink moved
    /// during every attempt to read one update whole.
    pub fn refresh(&self) -> Result<(), SecretDirError> {
        let serial = self
            .shared
            .refreshing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let snapshot = Arc::new(scan::scan(&self.shared.root)?);
        *self
            .shared
            .current
            .write()
            .unwrap_or_else(PoisonError::into_inner) = snapshot;
        drop(serial);
        Ok(())
    }

    /// Refreshes the directory on a thread every `interval` until the
    /// returned [`Watch`] drops, handing each failed refresh to `on_error`.
    ///
    /// A secret written to the directory is answered at most `interval`
    /// after the write reaches the directory.
    ///
    /// # Errors
    ///
    /// Returns [`SecretDirError::ZeroInterval`] for a zero `interval`, and
    /// [`SecretDirError::Watch`] when the thread cannot be started.
    pub fn watch<F>(&self, interval: Duration, mut on_error: F) -> Result<Watch, SecretDirError>
    where
        F: FnMut(&SecretDirError) + Send + 'static,
    {
        if interval.is_zero() {
            return Err(SecretDirError::ZeroInterval);
        }
        let (stop, stopped) = mpsc::channel::<()>();
        let dir = self.clone();
        let thread = thread::Builder::new()
            .name("ferrohealth-secret-dir".to_owned())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(interval) {
                    if let Err(error) = dir.refresh() {
                        on_error(&error);
                    }
                }
            })
            .map_err(|source| SecretDirError::Watch { source })?;
        Ok(Watch {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl fmt::Debug for SecretDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretDir")
            .field("root", &self.shared.root)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

/// The thread [`SecretDir::watch`] started; dropping it stops the thread
/// and waits for it.
#[derive(Debug)]
pub struct Watch {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            // NOTE: no specification governs this: our own design; a panic of
            // the caller's `on_error` ends only the watch, never the drop.
            drop(thread.join());
        }
    }
}

/// The secrets of one read of the directory.
pub struct Snapshot {
    entries: BTreeMap<String, Entry>,
}

enum Entry {
    Value(SecretString),
    Unreadable(Arc<io::Error>),
    NotUtf8,
    Empty,
}

impl Snapshot {
    /// The names of the secrets, sorted, a name whose file could not be read
    /// included.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Returns the secret `name`.
    ///
    /// A name is one path segment: not empty, no `/`, `\` or NUL, and no
    /// leading dot, so a name from a database row cannot reach outside the
    /// directory or a hidden file.
    ///
    /// # Errors
    ///
    /// Returns [`SecretDirError::InvalidName`] for a name that is not one
    /// path segment, [`SecretDirError::Missing`] when no file holds it, and
    /// [`SecretDirError::Read`], [`SecretDirError::NotUtf8`] or
    /// [`SecretDirError::Empty`] when its file could not be read as a secret.
    pub fn get(&self, name: &str) -> Result<SecretString, SecretDirError> {
        check_name(name)?;
        self.value(name)
    }

    /// Returns the client certificate `name`: the certificate in
    /// `<name>.crt` and its private key in `<name>.key`, from the same
    /// update.
    ///
    /// # Errors
    ///
    /// The errors of [`Snapshot::get`], for either file.
    pub fn get_pair(&self, name: &str) -> Result<ClientCertificate, SecretDirError> {
        check_name(name)?;
        let certificate = self.value(&format!("{name}.crt"))?;
        let key = self.value(&format!("{name}.key"))?;
        Ok(ClientCertificate {
            certificate: certificate.expose_secret().to_owned(),
            key,
        })
    }

    fn value(&self, name: &str) -> Result<SecretString, SecretDirError> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| SecretDirError::Missing {
                name: name.to_owned(),
            })?;
        match entry {
            Entry::Value(value) => Ok(value.clone()),
            Entry::Unreadable(source) => Err(SecretDirError::Read {
                name: name.to_owned(),
                source: Arc::clone(source),
            }),
            Entry::NotUtf8 => Err(SecretDirError::NotUtf8 {
                name: name.to_owned(),
            }),
            Entry::Empty => Err(SecretDirError::Empty {
                name: name.to_owned(),
            }),
        }
    }
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Snapshot")
            .field("names", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

fn check_name(name: &str) -> Result<(), SecretDirError> {
    if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', '\0']) {
        return Err(SecretDirError::InvalidName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// A client certificate and its private key, read by one name.
///
/// `Debug` shows neither.
pub struct ClientCertificate {
    certificate: String,
    key: SecretString,
}

impl ClientCertificate {
    /// The certificate (chain), as the `.crt` file holds it.
    #[must_use]
    pub fn certificate(&self) -> &str {
        &self.certificate
    }

    /// The private key, as the `.key` file holds it.
    #[must_use]
    pub fn key(&self) -> &SecretString {
        &self.key
    }
}

impl fmt::Debug for ClientCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCertificate")
            .field("certificate", &REDACTED)
            .field("key", &REDACTED)
            .finish()
    }
}

/// Why a secret directory or a secret in it could not be read.
///
/// The directory variants name the path and the secret variants the name;
/// none carries a value.
#[derive(Debug)]
#[non_exhaustive]
pub enum SecretDirError {
    /// The path given is not a directory.
    NotADirectory {
        /// The path.
        path: PathBuf,
    },
    /// The directory could not be read.
    Unreadable {
        /// The directory.
        path: PathBuf,
        /// What the file system reported.
        source: io::Error,
    },
    /// The directory's `..data` symlink moved during every attempt to read
    /// one update whole.
    Unstable {
        /// The directory.
        path: PathBuf,
    },
    /// The name is not one path segment.
    InvalidName {
        /// The name asked for.
        name: String,
    },
    /// No file holds the secret.
    Missing {
        /// The name of the secret.
        name: String,
    },
    /// The secret's file could not be read.
    Read {
        /// The name of the secret.
        name: String,
        /// What the file system reported when the snapshot was read.
        source: Arc<io::Error>,
    },
    /// The secret's file is not UTF-8 text.
    NotUtf8 {
        /// The name of the secret.
        name: String,
    },
    /// The secret's file holds nothing past its line ending.
    Empty {
        /// The name of the secret.
        name: String,
    },
    /// A watch was asked for with a zero interval.
    ZeroInterval,
    /// The watch thread could not be started.
    Watch {
        /// What the operating system reported.
        source: io::Error,
    },
}

impl fmt::Display for SecretDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory { path } => {
                write!(
                    f,
                    "the secret directory {} is not a directory",
                    path.display()
                )
            }
            Self::Unreadable { path, .. } => {
                write!(
                    f,
                    "the secret directory {} could not be read",
                    path.display()
                )
            }
            Self::Unstable { path } => write!(
                f,
                "the secret directory {} changed during every read",
                path.display()
            ),
            Self::InvalidName { name } => write!(
                f,
                "{name:?} is not a secret name: a name is one path segment with no leading dot"
            ),
            Self::Missing { name } => write!(f, "secret {name:?} is not in the secret directory"),
            Self::Read { name, .. } => write!(f, "secret {name:?} could not be read"),
            Self::NotUtf8 { name } => write!(f, "secret {name:?} is not UTF-8 text"),
            Self::Empty { name } => write!(f, "secret {name:?} is empty"),
            Self::ZeroInterval => {
                f.write_str("a secret directory is watched at an interval above zero")
            }
            Self::Watch { .. } => f.write_str("the secret directory watch could not be started"),
        }
    }
}

impl std::error::Error for SecretDirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreadable { source, .. } | Self::Watch { source } => Some(source),
            Self::Read { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// The configuration of a secret directory, for a product's configuration
/// file.
///
/// # Examples
///
/// ```
/// use ferrohealth_secret::dir::SecretDirConfig;
///
/// let config: SecretDirConfig = serde_json::from_str(r#"{"dir": "/run/secrets/sources"}"#)?;
/// assert_eq!(config.poll_seconds.interval().as_secs(), 10);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SecretDirConfig {
    /// The directory, one file per secret: a Kubernetes Secret mounted as a
    /// volume without `subPath`, or a bind mount.
    pub dir: PathBuf,
    /// Seconds between two reads of the directory.
    #[serde(default)]
    pub poll_seconds: PollSeconds,
}

/// The seconds between two reads of a secret directory, at least one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct PollSeconds(NonZeroU64);

impl PollSeconds {
    /// The interval of `seconds`.
    #[must_use]
    pub fn new(seconds: NonZeroU64) -> Self {
        Self(seconds)
    }

    /// The interval, for [`SecretDir::watch`].
    #[must_use]
    pub fn interval(self) -> Duration {
        Duration::from_secs(self.0.get())
    }
}

impl Default for PollSeconds {
    // NOTE: no specification governs this: our own design; ten seconds is
    // small beside the kubelet's own sync period and cache delay.
    fn default() -> Self {
        Self(NonZeroU64::MIN.saturating_add(9))
    }
}

#[cfg(test)]
mod tests {
    use super::{SecretDirError, check_name};

    #[test]
    fn a_name_is_one_path_segment_with_no_leading_dot() {
        for name in ["hmac", "client.crt", "a-b_c.1", "x..y"] {
            assert!(check_name(name).is_ok(), "{name}");
        }
        for name in [
            "", ".", "..", "..data", ".hidden", "a/b", "../x", "a\\b", "a\0b",
        ] {
            let error = check_name(name).expect_err("the name is refused");
            assert!(
                matches!(&error, SecretDirError::InvalidName { name: n } if n == name),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_invalid_name_renders_quoted() {
        let error = check_name("a/\nb").expect_err("the name is refused");
        assert_eq!(
            "\"a/\\nb\" is not a secret name: a name is one path segment with no leading dot",
            error.to_string()
        );
    }
}
