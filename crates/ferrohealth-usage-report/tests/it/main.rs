// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests over the public API of ferrohealth-usage-report.

// NOTE: no specification governs this: our own design; clippy reads the
// helpers of these tests as tests, as in the other crates' suites.
#![cfg(test)]

#[cfg(feature = "receive")]
mod received;
#[cfg(feature = "schema")]
mod schema;
