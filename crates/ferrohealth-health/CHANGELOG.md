<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-health

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-health-vX.Y.Z`.

## [Unreleased]

## [0.1.0] - 2026-10-26

### Added

- `document::Readiness` and `document::Summary`, the readiness document
  every server writes and every console reads: the overall state, the phase
  and every indicator's state, ready only while the process serves and no
  indicator is down; the default build is serde alone and compiles for
  `wasm32-unknown-unknown` (#16).
- `registry::Registry` and `registry::HealthIndicator` (feature `registry`):
  every indicator runs at once under a timeout, and one that times out or
  panics counts as down (#16).
- `lifecycle::Lifecycle` (feature `registry`): booting, serving and
  draining, moving forward only (#16).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-health-v0.1.0...HEAD
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-health-v0.1.0
