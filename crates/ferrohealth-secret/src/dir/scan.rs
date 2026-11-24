// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! One read of a secret directory, whole.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::SecretString;
use secrecy::zeroize::Zeroizing;

use super::{Entry, SecretDirError, Snapshot};
use crate::file::secret_text;

// NOTE: the kubelet's atomic writer; the symlink it renames to each new
// directory of keys, which is never changed once it is named.
const DATA_LINK: &str = "..data";

// NOTE: no specification governs this: our own design; a read retried past
// this many swaps reports the directory unstable.
const ATTEMPTS: usize = 8;

/// Why one attempt read no update whole.
enum Attempt {
    /// The directory `..data` named was replaced during the read.
    Moved,
    /// The directory could not be read.
    Failed(io::Error),
}

/// Reads every secret of `root`. With a `..data` symlink, the keys come
/// from the directory it names, read again until the link names the same
/// directory before and after the read.
pub(super) fn scan(root: &Path) -> Result<Snapshot, SecretDirError> {
    for _ in 0..ATTEMPTS {
        let Some(generation) = data_target(root)? else {
            return read_entries(root, false)
                .map(|entries| Snapshot { entries })
                .map_err(|attempt| failed(root, attempt));
        };
        match read_entries(&root.join(&generation), true) {
            Ok(entries) if data_target(root)?.as_ref() == Some(&generation) => {
                return Ok(Snapshot { entries });
            }
            Ok(_) | Err(Attempt::Moved) => {}
            Err(attempt) => return Err(failed(root, attempt)),
        }
    }
    Err(SecretDirError::Unstable {
        path: root.to_path_buf(),
    })
}

fn failed(root: &Path, attempt: Attempt) -> SecretDirError {
    let source = match attempt {
        Attempt::Failed(source) => source,
        Attempt::Moved => io::Error::from(io::ErrorKind::NotFound),
    };
    SecretDirError::Unreadable {
        path: root.to_path_buf(),
        source,
    }
}

/// The directory `..data` names, or `None` for a directory without one.
fn data_target(root: &Path) -> Result<Option<PathBuf>, SecretDirError> {
    match fs::read_link(root.join(DATA_LINK)) {
        Ok(target) => Ok(Some(target)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SecretDirError::Unreadable {
            path: root.to_path_buf(),
            source,
        }),
    }
}

/// Reads every secret file of `dir`. A file that vanished is a moved
/// generation when `generation` is set, and a removed secret otherwise.
fn read_entries(dir: &Path, generation: bool) -> Result<BTreeMap<String, Entry>, Attempt> {
    let vanished = |error: io::Error| {
        if generation && error.kind() == io::ErrorKind::NotFound {
            Attempt::Moved
        } else {
            Attempt::Failed(error)
        }
    };
    let mut entries = BTreeMap::new();
    for item in fs::read_dir(dir).map_err(vanished)? {
        let item = item.map_err(vanished)?;
        // NOTE: no specification governs this: our own design; a file name
        // that is not UTF-8 can never be asked for, and a dot file is hidden.
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        match read_entry(&item.path()) {
            Ok(Some(entry)) => {
                entries.insert(name, entry);
            }
            Err(Vanished) if generation => return Err(Attempt::Moved),
            Ok(None) | Err(Vanished) => {}
        }
    }
    Ok(entries)
}

/// Reads one entry: `None` for one that is no file, such as a nested
/// directory, and `Err` for one that vanished.
fn read_entry(path: &Path) -> Result<Option<Entry>, Vanished> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Vanished),
        Err(error) => return Ok(Some(Entry::Unreadable(Arc::new(error)))),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => Zeroizing::new(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Vanished),
        Err(error) => return Ok(Some(Entry::Unreadable(Arc::new(error)))),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(Some(Entry::NotUtf8));
    };
    Ok(Some(secret_text(text).map_or(Entry::Empty, |value| {
        Entry::Value(SecretString::from(value))
    })))
}

/// A file listed in the directory that was gone when it was read.
struct Vanished;
