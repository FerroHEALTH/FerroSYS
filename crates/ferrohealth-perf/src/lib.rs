// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The performance gate of the FerroHEALTH servers.
//!
//! A product's CI job starts its server and its PostgreSQL, then runs the
//! `ferrohealth-perf` binary against them. A run has two phases:
//!
//! 1. **Idle:** no request for a minute. The server's CPU time and the
//!    database's transactions per second are held to absolute limits,
//!    because a server at rest must do close to nothing.
//! 2. **Load:** the product's request set at a fixed open-loop rate, after
//!    a warm-up. Latency per request group, throughput, peak memory, CPU
//!    per request and transactions per request are compared with the
//!    committed baseline, each within its tolerance.
//!
//! The A/B mode ([`ab`]) measures the base of a change and its head side by
//! side on one host instead, and judges the head over the base, so no
//! baseline from another machine takes part.
//!
//! The modules, in the order a run uses them:
//!
//! - [`plan`]: `perf/plan.toml`, the limits, the load and the request set.
//! - [`process`] and [`database`]: the server's CPU and memory from `/proc`
//!   or its cgroup, and the database's transactions from
//!   `pg_stat_database`.
//! - [`load`]: the open-loop generator of HTTP requests and, through
//!   [`mllp`], HL7 v2 messages, with [`histogram`] and [`quantile`] for the
//!   latencies.
//! - [`report`], [`metric`] and [`fingerprint`]: the JSON report and what
//!   it measured on which host.
//! - [`baseline`] and [`compare`]: `perf/baseline.toml` and the judgement.
//! - [`summary`]: the Markdown step summary.
//! - [`run`]: one run, end to end, and [`ab`]: one A/B run.
//!
//! No specification governs the gate: our own design (#249).

pub mod ab;
pub mod baseline;
pub mod compare;
pub mod credentials;
pub mod database;
pub mod fingerprint;
pub mod histogram;
pub mod load;
pub mod metric;
pub mod mllp;
pub mod plan;
pub mod process;
pub mod quantile;
pub mod report;
pub mod run;
pub mod summary;
