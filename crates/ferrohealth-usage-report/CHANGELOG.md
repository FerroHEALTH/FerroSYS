<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-usage-report

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-usage-report-vX.Y.Z`.

## [Unreleased]

## [0.2.0] - 2026-10-27

### Changed

- Breaking: `window::Window::new` takes any
  `Fn(&Method, Option<&str>) -> Option<Key> + Send + Sync + 'static`, so a
  grouping may capture state, such as the configurable mount a product's
  routes sit under, and group by the part of the template below it. A
  product that passed a `fn` item or a non-capturing closure compiles
  unchanged; one that named `window::Grouping` names the closure bound
  instead. `Window` is no longer `UnwindSafe` or `RefUnwindSafe` (#124).

### Removed

- Breaking: `window::Grouping`, the function-pointer type of a grouping
  (#124).

## [0.1.2] - 2026-10-26

### Added

- The `telemetry` feature: `window::Window` implements
  `ferrohealth_telemetry::request_log::Recorder`, so a product hands its
  window to the shared request log with `RequestLog::with_recorder` and
  writes no adapter of its own (#117).

## [0.1.1] - 2026-10-26

### Added

- `state::Store`, the state the report keeps across restarts and
  processes: the installation id, the start claim, the daily claim and the
  counts of every process since the last daily report. `StateDir`
  implements it for one process; a product whose replicas share a database
  implements it there, so the replicas send one start report, one daily
  report a day and one installation id (#90).
- `reporter::Reporter::with_store` and `reporter::start_with_store`, the
  reporter over a `Store` (#90).
- `reporter::ReportError::SentElsewhere`, the answer of `send_daily` when
  another process of the installation sent the daily report, and
  `ReportError::Store` (#90).

### Changed

- `send_daily` hands the window's counts to the store before it sends, so
  over a shared store the daily report carries every process's counts; over
  `StateDir` nothing changes (#90).

## [0.1.0] - 2026-10-26

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

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-usage-report-v0.2.0...HEAD
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-usage-report-v0.1.2...ferrohealth-usage-report-v0.2.0
[0.1.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-usage-report-v0.1.1...ferrohealth-usage-report-v0.1.2
[0.1.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-usage-report-v0.1.0...ferrohealth-usage-report-v0.1.1
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-usage-report-v0.1.0
