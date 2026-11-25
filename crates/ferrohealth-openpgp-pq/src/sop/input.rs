// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Indirect inputs: a file name, or a special designator (§7.1).

use std::ffi::OsStr;
use std::io::Read as _;
use std::path::Path;

use crate::sop::exit::{Exit, Failure};

/// Reads the indirect input `name`: `@ENV:VAR` reads the environment
/// variable (§7.1.1), `@FD:n` the open file descriptor `n` (§7.1.2), and
/// any other name the file of that name. Another `@` prefix is refused
/// (§7.1), and so is a designator that is also the name of a file (§11.9).
pub(crate) fn read(name: &OsStr) -> Result<Vec<u8>, Failure> {
    let Some(text) = name.to_str() else {
        return read_file(Path::new(name));
    };
    if !text.starts_with('@') {
        return read_file(Path::new(name));
    }
    if Path::new(name).exists() {
        return Err(Failure::new(
            Exit::AmbiguousInput,
            format!("{text} is a special designator and also the name of a file"),
        ));
    }
    if let Some(variable) = text.strip_prefix("@ENV:") {
        return std::env::var_os(variable)
            .map(std::ffi::OsString::into_encoded_bytes)
            .ok_or_else(|| {
                Failure::new(
                    Exit::MissingInput,
                    format!("the environment variable {variable} is not set"),
                )
            });
    }
    if let Some(descriptor) = text.strip_prefix("@FD:") {
        let Ok(number) = descriptor.parse::<u32>() else {
            return Err(Failure::new(
                Exit::UnsupportedSpecialPrefix,
                format!("{text} names no file descriptor"),
            ));
        };
        // NOTE: §7.1.2, a descriptor is read through its /dev/fd entry, which macOS and Linux provide.
        return read_file(Path::new(&format!("/dev/fd/{number}")));
    }
    Err(Failure::new(
        Exit::UnsupportedSpecialPrefix,
        format!("{text} has a special prefix this implementation does not handle"),
    ))
}

/// Reads all of standard input.
pub(crate) fn stdin() -> Result<Vec<u8>, Failure> {
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data).map_err(|error| {
        Failure::new(
            Exit::UnspecifiedFailure,
            format!("reading standard input: {error}"),
        )
    })?;
    Ok(data)
}

fn read_file(path: &Path) -> Result<Vec<u8>, Failure> {
    std::fs::read(path).map_err(|error| {
        let exit = if error.kind() == std::io::ErrorKind::NotFound {
            Exit::MissingInput
        } else {
            Exit::UnspecifiedFailure
        };
        Failure::new(exit, format!("reading {}: {error}", path.display()))
    })
}

/// The bytes a PASSWORD input is tried as when it unlocks a key (§11.8.2):
/// as given, then without its trailing whitespace when that differs.
pub(crate) fn password_variants(password: &[u8]) -> Vec<Vec<u8>> {
    let mut variants = vec![password.to_vec()];
    if let Ok(text) = std::str::from_utf8(password) {
        let trimmed = text.trim_end();
        if trimmed.len() != text.len() {
            variants.push(trimmed.as_bytes().to_vec());
        }
    }
    variants
}

/// A password that locks new material (§11.8.1): UTF-8, with its trailing
/// whitespace trimmed.
pub(crate) fn new_password(password: &[u8]) -> Result<String, Failure> {
    std::str::from_utf8(password)
        .map(|text| text.trim_end().to_owned())
        .map_err(|_utf8| {
            Failure::new(
                Exit::PasswordNotHumanReadable,
                "the password is not UTF-8 (draft-dkg-openpgp-stateless-cli-16 §11.8.1)",
            )
        })
}
