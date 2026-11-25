// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The return codes of draft-dkg-openpgp-stateless-cli-16 §8 that
//! `ferrohealth-sop` returns, and the failure that carries one.

use std::fmt;

use ferrohealth_openpgp_pq::error::Error;

/// A return code of §8, by its mnemonic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    /// An otherwise unspecified failure.
    UnspecifiedFailure,
    /// No acceptable signatures found.
    NoSignature,
    /// An asymmetric algorithm other than ML-DSA-65+Ed25519.
    UnsupportedAsymmetricAlgo,
    /// A required argument is missing.
    MissingArg,
    /// A password that is not UTF-8.
    PasswordNotHumanReadable,
    /// An option this implementation does not handle.
    UnsupportedOption,
    /// Input of the wrong type or form.
    BadData,
    /// Non-text input where text is expected.
    ExpectedText,
    /// An input file does not exist.
    MissingInput,
    /// No password unlocks a KEYS input.
    KeyIsProtected,
    /// A subcommand this implementation does not handle.
    UnsupportedSubcommand,
    /// An `@` designator whose prefix is not `@ENV:` or `@FD:`.
    UnsupportedSpecialPrefix,
    /// An `@` designator that is also the name of a file.
    AmbiguousInput,
    /// A key that may not sign.
    KeyCannotSign,
    /// Options that exclude each other.
    IncompatibleOptions,
    /// A profile this implementation does not offer.
    UnsupportedProfile,
}

impl Exit {
    /// The numeric value of §8 Table 1.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::UnspecifiedFailure => 1,
            Self::NoSignature => 3,
            Self::UnsupportedAsymmetricAlgo => 13,
            Self::MissingArg => 19,
            Self::PasswordNotHumanReadable => 31,
            Self::UnsupportedOption => 37,
            Self::BadData => 41,
            Self::ExpectedText => 53,
            Self::MissingInput => 61,
            Self::KeyIsProtected => 67,
            Self::UnsupportedSubcommand => 69,
            Self::UnsupportedSpecialPrefix => 71,
            Self::AmbiguousInput => 73,
            Self::KeyCannotSign => 79,
            Self::IncompatibleOptions => 83,
            Self::UnsupportedProfile => 89,
        }
    }
}

/// A failed invocation: the return code and the message for standard error.
#[derive(Debug)]
pub(crate) struct Failure {
    exit: Exit,
    message: String,
}

impl Failure {
    pub(crate) fn new(exit: Exit, message: impl Into<String>) -> Self {
        Self {
            exit,
            message: message.into(),
        }
    }

    /// A library error met while reading or using `what`, with the return
    /// code its kind takes.
    pub(crate) fn from_error(what: &str, error: &Error) -> Self {
        let exit = match error {
            Error::Algorithm { .. } | Error::KeyVersion { .. } => Exit::UnsupportedAsymmetricAlgo,
            Error::NoDirectKeySignature
            | Error::Revoked
            | Error::CannotSign
            | Error::OutsideValidity => Exit::KeyCannotSign,
            Error::Locked(_) => Exit::KeyIsProtected,
            Error::Provider { .. } => Exit::UnspecifiedFailure,
            _ => Exit::BadData,
        };
        Self::new(exit, format!("{what}: {}", chain(error)))
    }

    pub(crate) fn exit(&self) -> Exit {
        self.exit
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// `error` and each cause under it, joined.
pub(crate) fn chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
