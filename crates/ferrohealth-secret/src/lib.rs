// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The secret and redaction types of the FerroHEALTH products.
//!
//! Redaction is a property of the type, so a secret cannot reach a log line,
//! a `Debug` rendering, an error message or a rendered configuration by
//! accident:
//!
//! - [`secret::Secret`] holds a credential (a password, a client secret, a
//!   token) and renders as [`REDACTED`] everywhere.
//! - [`secret_url::SecretUrl`] holds a URL or connection string that may
//!   carry a credential and renders with its userinfo and query masked;
//!   [`secret_url::RedactedUrl`] gives any URL the same rendering.
//! - [`redacted::Redacted`] gives a value that holds clinical content or a
//!   patient identifier a `Debug` that shows none of it.
//! - [`mod@file`] reads a secret from its key or from the file its `<key>_file`
//!   sibling names, exactly one of the two.
//!
//! Every secret is held in a [`secrecy::SecretString`], which zeroes its
//! memory on drop. No specification governs any of this: our own design.

pub mod file;
pub mod redacted;
pub mod secret;
pub mod secret_url;

/// The fixed text every rendering shows in place of a secret or of a value
/// that may name a patient.
pub const REDACTED: &str = "***";
