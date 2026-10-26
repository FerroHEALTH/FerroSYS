// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The installation report of the FerroHEALTH products to FerroPULSE.
//!
//! Every product sends the same two reports on FerroPULSE's report envelope
//! v2: one `start` report at each start and one `daily` report a day. Each
//! carries the installation's random id, the version, the licence grant,
//! the deployment kind, coarse host size and the product's own `status`;
//! the daily report adds latency and error counts per request group. No
//! clinical content, identifier, URL or request path is ever part of one.
//!
//! - [`section::UsageReportSection`]: the `[usage_report]` section, with its
//!   off switch.
//! - [`state::StateDir`]: the instance id under `state_dir`, created once.
//! - [`window::Window`]: the latency window the request log fills.
//! - [`envelope`]: the closed types of envelope v2.
//! - [`reporter::Reporter`]: assembles and sends the reports; a product
//!   supplies only its [`reporter::ProductSection`].
//!
//! The contract is FerroPULSE's report envelope v2, vendored under
//! `docs/specs/ferropulse/`; no other specification governs the report.

pub mod envelope;
pub mod reporter;
pub mod section;
pub mod state;
pub mod window;
