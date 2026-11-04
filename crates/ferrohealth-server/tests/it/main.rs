// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests over the public API of ferrohealth-server.

// NOTE: no specification governs this: our own design; the tests moved from the
// inline test module keep its context, so clippy reads them as tests.
#![cfg(test)]

mod stack;
