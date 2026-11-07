// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests over the public API and the binary of ferrohealth-perf.

// NOTE: no specification governs this: our own design; the helpers beside the
// tests share their context, so clippy reads them as tests.
#![cfg(test)]

mod bindings;
mod cli;
mod load;
mod plant;
mod support;
