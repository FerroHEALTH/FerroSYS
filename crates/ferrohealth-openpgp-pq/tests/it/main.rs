// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The integration tests of `ferrohealth-openpgp-pq`.

// NOTE: no specification governs this: our own design; the crate is built as
// a test, so clippy reads its fixtures as tests.
#![cfg(test)]

mod refusals;
mod roundtrip;
#[cfg(feature = "cli")]
mod sop;
mod support;
mod vectors;
