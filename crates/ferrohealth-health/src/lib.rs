// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The health family of the FerroHEALTH products.
//!
//! Every product serves `/health`, `/health/liveness` and
//! `/health/readiness` under its base path. Liveness answers while the
//! process serves and checks nothing. Readiness answers with the
//! [`document::Readiness`] document: the process's [`document::Phase`] and
//! the state of every indicator, ready only while the process is serving and
//! no indicator is down. `/health` answers the same status alone
//! ([`document::Summary`]).
//!
//! The default build is the document alone, serde and nothing else, so it
//! compiles for `wasm32-unknown-unknown` and a console's browser half reads
//! the document its server writes. The `registry` feature adds what a
//! server runs: the [`registry::HealthIndicator`] trait, the
//! [`registry::Registry`] that runs every indicator concurrently under a
//! timeout, and the [`lifecycle::Lifecycle`] that withdraws readiness the
//! moment a stop is requested.
//!
//! No specification governs health probes: our own design.

pub mod document;
#[cfg(feature = "registry")]
pub mod lifecycle;
#[cfg(feature = "registry")]
pub mod registry;
