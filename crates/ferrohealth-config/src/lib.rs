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
//! 3. the environment: `<PREFIX>__SECTION__KEY` sets `[section] key` and
//!    `<PREFIX>__KEY` a top-level key; a product's fallbacks (such as
//!    `DATABASE_URL`) sit below them and its aliases (such as `RUST_LOG`)
//!    above;
//! 4. the `--set key=value` arguments, last.
//!
//! An array of tables, such as `[[auth.issuer]]`, is set by element: a
//! segment of digits after the array's key is the element's index, so
//! `<PREFIX>__AUTH__ISSUER__0__URL` sets `url` of the first element and
//! `--set auth.issuer.0.url=…` (or `auth.issuer[0].url`) does the same.
//! An index addresses the array as the layers below set it: an element the
//! file holds is overridden key by key, and the next index past the end
//! adds an element. The indices of all overrides together number the
//! elements from 0 without a gap, in any order; an index with a leading
//! zero (`01`), one that skips an element, and one under a key that holds
//! no array of tables are refused. An index as the last segment
//! (`<PREFIX>__AUTH__ISSUER__1`) sets the whole element as an inline
//! table, and the whole array is still set as one TOML array, which the
//! indexed overrides then patch. Each element is read as strictly as the
//! file: an unknown key is refused, and a secret's `<key>_file` sibling is
//! set the same way.
//!
//! The load is strict. An unknown key, in the file or through an override,
//! and a value of the wrong type are refused with the dotted key and, for an
//! unknown key, the closest known one. Only the override form
//! `<PREFIX>__…` is checked against the tree; every other variable under the
//! prefix is left alone, because product prefixes nest (`FERROTERM` and
//! `FERROTERM_CONSOLE`) and such a name may belong to another binary.
//!
//! [`loader::Loaded`] keeps where each key came from
//! ([`provenance::Provenance`]), and [`report::check_report`] renders the
//! running tree for `config check`. Secrets are held in the redacting types
//! of `ferrohealth-secret`, so the rendering shows none of them. No
//! specification governs any of this: our own design.

pub mod error;
pub mod libpq;
pub mod loader;
mod overrides;
pub mod provenance;
pub mod removed;
pub mod report;
pub mod strict;
pub mod tls;
pub mod units;
