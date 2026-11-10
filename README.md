<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->
<div align="center">

<img src="assets/brand/ferrosys-lockup-auto.svg" alt="FerroSYS" width="290">

**The system plumbing every FerroHEALTH server shares, as Rust libraries: secrets, configuration, logging and telemetry, health checks, the HTTP server, the usage report and the performance gate.**

[Family site](https://ferrohealth.eu/) ·
[The crates](#the-crates) ·
[Install](#install) ·
[Licensing](https://ferrohealth.eu/docs/licensing.html)

**Project**<br>
[![License: BUSL-1.1](https://img.shields.io/badge/License-BUSL--1.1-blue)](LICENSE)
[![CI](https://github.com/FerroHEALTH/FerroSYS/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/FerroHEALTH/FerroSYS/actions/workflows/ci.yml)
[![Last commit](https://img.shields.io/github/last-commit/FerroHEALTH/FerroSYS?logo=github&label=last%20commit)](https://github.com/FerroHEALTH/FerroSYS/commits/main)
[![Open issues](https://img.shields.io/github/issues/FerroHEALTH/FerroSYS?logo=github&label=issues)](https://github.com/FerroHEALTH/FerroSYS/issues)
[![Family](https://img.shields.io/badge/family-FerroHEALTH-3F6070?logo=rust&logoColor=white)](https://ferrohealth.eu/)

**Quality and security**<br>
[![Security policy](https://img.shields.io/badge/security-policy-brightgreen?logo=github)](SECURITY.md)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/FerroHEALTH/FerroSYS/badge)](https://scorecard.dev/viewer/?uri=github.com/FerroHEALTH/FerroSYS)

</div>

## What FerroSYS is

Every FerroHEALTH server has to do the same handful of things before it can
do its real job. It writes a log that a person or a log pipeline can read.
It sends traces and measurements to the monitoring system a hospital already
runs. It tells the container platform whether it is alive and ready for
work. It stops cleanly when asked to, without dropping a request halfway. It
reads its settings from a file and the environment, and keeps passwords out
of every log line.

FerroSYS is that code, written once as Rust library crates that FerroEHR,
FerroBRIDGE, FerroFED, FerroPIX and FerroTERM all link. Every server
therefore logs, configures, reports its health and shuts down the same way,
and an operator learns it once.

The crates touch no clinical content. They carry how a server runs, never
what it stores or translates. FerroSYS distributes no configuration, deploys
nothing and runs no service of its own.

### Who it is for

- **Hospital IT and operations teams**, who run several FerroHEALTH servers
  and want one log format, one set of health checks and one way to configure
  each of them.
- **Developers of the FerroHEALTH servers**, who take these crates instead
  of writing the plumbing again.

## The crates

Each crate is published on crates.io on its own version line and has its
own README, which is its crates.io page.

| Crate | Version | What it provides | Features |
| --- | --- | --- | --- |
| [`ferrohealth-secret`](crates/ferrohealth-secret/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-secret)](https://crates.io/crates/ferrohealth-secret) | Secrets that render as `***` everywhere, URLs with their credentials masked, a `Redacted<T>` wrapper for clinical values, and the `<key>` or `<key>_file` rule for reading a secret. | `schema` |
| [`ferrohealth-config`](crates/ferrohealth-config/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-config)](https://crates.io/crates/ferrohealth-config) | The configuration loader: defaults, a TOML file, `<PREFIX>__SECTION__KEY` environment overrides and `--set`, read strictly with the closest known key suggested, provenance per key, and the `config check` rendering. | `schema` |
| [`ferrohealth-telemetry`](crates/ferrohealth-telemetry/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-telemetry)](https://crates.io/crates/ferrohealth-telemetry) | The `[telemetry]` section, the `pretty` or `json` console, the request log that names the route template and never carries a body, the panic hook, and one OpenTelemetry provider for traces and metrics. | `otlp`, `prometheus`, `axum`, `schema` |
| [`ferrohealth-health`](crates/ferrohealth-health/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-health)](https://crates.io/crates/ferrohealth-health) | The readiness document every server and console shares, and the indicator registry and process lifecycle a server runs. The default build compiles for `wasm32-unknown-unknown`. | `registry` |
| [`ferrohealth-server`](crates/ferrohealth-server/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-server)](https://crates.io/crates/ferrohealth-server) | The HTTP stack (request id, panic catch, timeout, body limit, overload bound, API security headers), the health routes, the listener with TLS and the bounded drain on `SIGTERM`, the container healthcheck, the `serve`, `config check` and `healthcheck` subcommands, and RFC 9457 problem details. | `console`, `reqwest`, `platform-verifier`, `otlp`, `revocation`, `schema` |
| [`ferrohealth-usage-report`](crates/ferrohealth-usage-report/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-usage-report)](https://crates.io/crates/ferrohealth-usage-report) | The installation report to FerroPULSE on envelope v2: the random instance id, the latency window per request group, and the start and daily reporter. No clinical content, identifier, URL or request path is ever part of a report. | `telemetry`, `receive`, `schema` |
| [`ferrohealth-perf`](crates/ferrohealth-perf/README.md) | [![crates.io](https://img.shields.io/crates/v/ferrohealth-perf)](https://crates.io/crates/ferrohealth-perf) | The performance gate a product's CI job runs against its server: idle CPU and database transactions at rest, then open-loop load latency, memory and CPU per request, judged against the change's base on the same host or a committed baseline. A binary that no product links. | none |

Every `schema` feature implements `schemars::JsonSchema` on the crate's
configuration types, for a product that generates its configuration
reference from its types.

## Install

The crates need Rust 1.98 or newer (edition 2024). Every FerroHEALTH
product pins each crate exactly and moves one pin at a time, so its
lockfile alone decides the build:

```toml
[dependencies]
ferrohealth-secret = "=0.2.1"
ferrohealth-config = "=0.4.4"
ferrohealth-telemetry = { version = "=0.4.1", features = ["otlp", "prometheus", "axum"] }
ferrohealth-health = { version = "=0.1.1", features = ["registry"] }
ferrohealth-server = { version = "=0.8.4", features = ["otlp"] }
ferrohealth-usage-report = { version = "=0.4.1", features = ["telemetry"] }
```

The performance gate is a binary:

```sh
cargo install ferrohealth-perf --version =0.9.0 --locked
```

The release plan of each crate refuses a tag while this page or the crate's
own README pins another version, so the lines above are the released ones.

## Usage

A product describes its configuration as one serde tree. The loader reads
it from the defaults, the file and the environment, and a secret in it
never renders:

```rust
use ferrohealth_config::loader::Loader;
use ferrohealth_secret::secret::Secret;

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    listen: String,
    password: Secret,
}

fn load() -> Result<Config, ferrohealth_config::error::ConfigError> {
    let loader = Loader::new("MYSERVER")?.alias("RUST_LOG", "telemetry.filter");
    Ok(loader.load::<Config>(None)?.into_parts().0)
}
```

With `MYSERVER__LISTEN=0.0.0.0:8080` the loader sets `listen`, and an
unknown key such as `MYSERVER__LISTN` is refused with the closest known
key. CI compiles this example as a doctest of `ferrohealth-config`.

Each crate's README shows the rest: the stack and the run path in
[`ferrohealth-server`](crates/ferrohealth-server/README.md), the request log
and the OpenTelemetry provider in
[`ferrohealth-telemetry`](crates/ferrohealth-telemetry/README.md), and the
plan, the baseline and the CI wiring of the performance gate in
[`ferrohealth-perf`](crates/ferrohealth-perf/README.md).

## What an operator gets

Every FerroHEALTH server built on these crates behaves the same way:

- **Configuration:** one TOML file, environment overrides of the form
  `<PRODUCT>__SECTION__KEY`, `--set key=value`, and `<key>_file` for every
  secret. `config check` prints each key with the place it came from and
  every secret as `***`.
- **Logs:** `pretty` lines for a person or `json` lines for a pipeline, one
  line per request with the method, the route template, the status, the
  latency and the server's request id.
- **Telemetry:** traces and metrics over OTLP to the collector you already
  run, with TLS, and a Prometheus endpoint.
- **Health:** `/health`, `/health/liveness` and `/health/readiness`, and a
  `healthcheck` subcommand for the container probe.
- **Shutdown:** on `SIGTERM` the server withdraws readiness, waits the drain
  delay and gives requests in flight a bounded time to finish.

## Security

- No secret, token, password or clinical payload reaches a log line, a
  `Debug` rendering or an error message. Every secret type redacts itself,
  and a URL is rendered with its userinfo masked.
- The request log never carries a body, the raw path, a header value or the
  client's request id.
- The crates contain no `unsafe` code (`unsafe_code = "forbid"`), and
  `cargo-deny` checks every dependency on every pull request.
- Report a vulnerability privately as the [security policy](SECURITY.md)
  describes. The policy every FerroHEALTH product shares is on the
  [family site](https://ferrohealth.eu/docs/security.html).

## Versioning

Each crate follows [Cargo's SemVer rules](https://doc.rust-lang.org/cargo/reference/semver.html)
on its own version line and is released by its own tag, `<crate>-vX.Y.Z`,
with its own `CHANGELOG.md`. On a `0.x` line a public API break moves the
minor version. Every pull request runs
[`cargo-semver-checks`](https://github.com/obi1kenobi/cargo-semver-checks)
against each changed crate's newest release on crates.io, with the default
features and with none, and refuses a break the manifest version does not
allow. The release process is in [`docs/release.md`](docs/release.md).

## Documentation

- The API reference of each crate on docs.rs:
  [secret](https://docs.rs/ferrohealth-secret),
  [config](https://docs.rs/ferrohealth-config),
  [telemetry](https://docs.rs/ferrohealth-telemetry),
  [health](https://docs.rs/ferrohealth-health),
  [server](https://docs.rs/ferrohealth-server),
  [usage-report](https://docs.rs/ferrohealth-usage-report) and
  [perf](https://docs.rs/ferrohealth-perf).
- The [architecture](docs/architecture.md), the design of record.
- The pinned tools and standards in [`docs/VERSIONS.md`](docs/VERSIONS.md).
- The [FerroHEALTH family book](https://ferrohealth.eu/docs/).

## Part of FerroHEALTH

FerroHEALTH is five servers, each its own product, that reach each other
only over published standards: FerroEHR stores the record, FerroBRIDGE
translates it to FHIR and the OMOP research model, FerroFED connects records
across organisations and borders, FerroPIX matches patient identities, and
FerroTERM serves the code systems.

Work that every server needs lives in shared libraries, so it is written
once: FerroAUTH for sign-in and access control, FerroAUDIT for the audit
trail, FerroUI for the web consoles, and FerroSYS for the system plumbing.

## Support

Ask a question or report a bug on the
[tracker](https://github.com/FerroHEALTH/FerroSYS/issues). Read
[CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request;
[MAINTAINERS.md](MAINTAINERS.md) names who reviews it. The brand assets and
their rules are in [`assets/brand`](assets/brand/README.md).

## Licence

Business Source License 1.1 ([LICENSE](LICENSE), [NOTICE](NOTICE)), with
Apache License 2.0 as the Change License four years after each version.
Non-production use and non-commercial production use are free; the
[licensing page](https://ferrohealth.eu/docs/licensing.html) says when you
need a commercial licence from Cadasto B.V.
