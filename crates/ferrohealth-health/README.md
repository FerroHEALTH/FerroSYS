<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-health

The health family of the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). Every product serves
`/health`, `/health/liveness` and `/health/readiness`, and every console
reads the same readiness document.

## What it does

- `document::Readiness` is the readiness document: the overall state (`up`,
  `degraded` or `down`), the process's phase (`booting`, `serving` or
  `draining`), and every indicator's state, sorted by name. The process is
  ready only while it serves and no indicator is down. `document::Summary`
  is the status alone, as `/health` answers it, and
  `Readiness::without_detail` is the document a route anyone can reach
  serves: the names and states, no detail. The default build is serde and
  nothing else, and compiles for `wasm32-unknown-unknown`.
- With the `registry` feature, `registry::Registry` runs every
  `HealthIndicator` at once, each bounded by a timeout (2 s by default). A
  check that times out or panics counts as down. An indicator whose
  `required` answers `false` covers a subsystem the process serves without:
  its row keeps the state it found, and it counts at most as degraded, so it
  never takes the process out of rotation. `Outcome::state` and
  `Outcome::detail` read what a check found.
- With the `registry` feature, `lifecycle::Lifecycle` moves from booting to
  serving to draining, and withdraws readiness the moment a stop is
  requested.

## Use

```toml
[dependencies]
ferrohealth-health = { version = "=0.1.0", features = ["registry"] }
```

A console's browser half and an operator-api crate take the default build.
Pin the version exactly, as every FerroHEALTH product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
