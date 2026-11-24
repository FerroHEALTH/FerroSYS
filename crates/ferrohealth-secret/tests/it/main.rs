// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The integration tests of `ferrohealth-secret`.

// NOTE: no specification governs this: our own design; the crate is built as
// a test, so clippy reads its fixtures as tests.
#![cfg(test)]

#[cfg(unix)]
mod dir;
#[cfg(feature = "schema")]
mod schema;
