// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The configuration loader of the FerroHEALTH products.
//!
//! A product describes its configuration as one serde tree whose every
//! struct carries `#[serde(deny_unknown_fields)]` and whose defaults live in
//! its `Default` impls. [`loader::Loader`] reads that tree from three layers,
//! each over the one before:
//!
//! 1. the defaults of the tree;
//! 2. an optional TOML file, named by the caller or by `<PREFIX>_CONFIG`;
//! 3. the environment: `<PREFIX>__SECTION__KEY` sets `[section] key`, and a
//!    product's aliases (such as `RUST_LOG`) set the keys they name.
//!
//! The load is strict. An unknown key, in the file or through an override,
//! and a value of the wrong type are refused with the dotted key and, for an
//! unknown key, the closest known one; a variable under the prefix that is
//! neither an override, an alias nor a name the product reserved is refused
//! too, so a variable that is set and never read cannot exist.
//!
//! [`loader::Loaded`] keeps where each key came from
//! ([`provenance::Provenance`]), and [`report::check_report`] renders the
//! running tree for `config check`. Secrets are held in the redacting types
//! of `ferrohealth-secret`, so the rendering shows none of them. No
//! specification governs any of this: our own design.

pub mod error;
pub mod loader;
mod overrides;
pub mod provenance;
pub mod report;
pub mod strict;
