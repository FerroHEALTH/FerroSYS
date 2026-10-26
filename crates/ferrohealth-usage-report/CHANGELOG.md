<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-usage-report

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-usage-report-vX.Y.Z`.

## [Unreleased]

### Added

- `section::UsageReportSection`, the `[usage_report]` section with its off
  switch and a collector refused at load unless it is `https` or loopback
  `http` with no userinfo, and `section::ci_of`, the CI detection (#18).
- `state::StateDir`, the instance id created once under `state_dir` and
  the ten-minute bound between start reports (#18).
- `window::Window`, the latency histogram and server errors per request
  group and the product's counters; the quantiles never exceed the slowest
  request seen (#18).
- `envelope`, the closed types of FerroPULSE's report envelope v2, which
  every report the tests build validates against (#18).
- `reporter::Reporter` and `reporter::ProductSection`: the start and daily
  reports, a refusal logged with the collector's status, and a daily report
  that does not arrive keeping its counts for the next try (#18).
