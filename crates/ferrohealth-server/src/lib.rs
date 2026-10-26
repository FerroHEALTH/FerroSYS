// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The HTTP server plumbing of the FerroHEALTH products.
//!
//! [`stack::app`] wraps a product's router in the one stack every server
//! runs, under its base path:
//!
//! - [`request_id`]: the server mints its own id for every request and logs
//!   it; a client's `X-Request-Id` is echoed and never logged.
//! - [`mod@panic`]: a handler panic becomes a `500` with a body the product
//!   supplies, and the panic message reaches neither the body nor the log.
//! - the request timeout (`408`) and the body limit (`413`);
//! - [`overload`]: past `max_concurrent_requests` a request is answered
//!   `503` with `Retry-After`, and the health routes are never refused;
//! - [`client_address`]: the address a request came from, read behind the
//!   trusted proxies;
//! - gzip and Brotli response compression, on by default;
//! - the request log of `ferrohealth-telemetry`;
//! - [`health`]: `/health`, `/health/liveness`, `/health/readiness` and,
//!   when the product supplies it, `/health/info`.
//!
//! [`caller_rate`] is the per-caller rate limit a product mounts inside its
//! authentication.
//!
//! [`serve::serve`] runs the app until `SIGTERM` or `SIGINT`, then withdraws
//! readiness, waits the drain delay and finishes the requests in flight
//! within a bound, over TLS when `[server.tls]` is set ([`tls`]);
//! [`serve::run_all`] serves several listeners under one signal.
//! [`tls_client`] reads the TLS material of a product's outbound calls.
//! [`healthcheck`] is the container probe, [`cli::Common`] and
//! [`cli::ConfigArgs`] the subcommands and arguments every binary shares,
//! [`build_info`] the build facts and the boot banner,
//! and `console` (feature `console`) the browser security headers of an
//! operator console, with `same_origin` the check of its state-changing
//! requests.
//!
//! No specification governs the stack: our own design, with the status
//! codes and `Retry-After` of RFC 9110.

pub mod build_info;
pub mod caller_rate;
pub mod cli;
pub mod client_address;
#[cfg(feature = "console")]
pub mod console;
pub mod health;
pub mod healthcheck;
pub mod overload;
pub mod panic;
pub mod refusal;
pub mod request_id;
#[cfg(feature = "console")]
pub mod same_origin;
pub mod section;
pub mod serve;
pub mod stack;
pub mod tls;
pub mod tls_client;
