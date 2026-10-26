// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The telemetry of the FerroHEALTH products.
//!
//! - [`section::TelemetrySection`] is the `[telemetry]` section every
//!   product embeds in its configuration; an invalid log filter is refused
//!   when the configuration is read.
//! - [`console`] renders `tracing` as `pretty` lines for a person or `json`
//!   lines for a log pipeline, one record per line.
//! - [`telemetry::Telemetry`] is the one OpenTelemetry provider of a
//!   process: one meter provider read by Prometheus and, when a collector is
//!   configured, pushed over OTLP, and one tracer provider that exports the
//!   product's own spans. A client's `traceparent` is never read: every
//!   request starts a trace of the server's own.
//! - [`panic::install_hook`] replaces Rust's panic hook with one fixed log
//!   line that leaves the panic message out, since a message can quote
//!   whatever value the thread held.
//! - `request_log` (feature `axum`) writes one line and opens one span per
//!   request: the method, the matched route template, the status, the
//!   latency and the server's request id, never a body, the raw path, a
//!   header value or the client's request id. It records the
//!   `http.server.request.duration` histogram, and writes the fields a
//!   handler adds to its response.
//!
//! No specification governs the console, the request log or the panic
//! line: our own design. The export follows the OpenTelemetry protocol and
//! its semantic conventions for the resource and the HTTP span.

pub mod console;
pub mod panic;
#[cfg(feature = "axum")]
pub mod request_log;
pub mod section;
pub mod telemetry;
