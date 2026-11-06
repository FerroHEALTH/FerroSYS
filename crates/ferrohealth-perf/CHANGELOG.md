<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-perf

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-perf-vX.Y.Z`.

## [Unreleased]

## [0.2.0] - 2026-11-07

### Changed

- Breaking: `perf/baseline.toml` holds one `[[baseline]]` entry per CPU
  model, keyed by the model string of the host fingerprint, because shared
  CI runners mix CPU generations and a load metric is comparable only on a
  like CPU (#258). `run` and `compare` judge the load phase against the
  entry for the runner's model. A model with no entry cannot be judged and
  exits 2, naming the model, the models the file holds and how to add the
  entry; the idle limits and the failed requests still judge that run, and
  a breach still exits 1. `--update` writes or tightens the entry for the
  runner's model alone and leaves every other entry as it was. The file
  moves to schema 2, and a schema-1 file is read as one entry of the model
  its host names, so a committed baseline keeps working. In the library,
  `baseline::Baselines` is the file with `read`, `parse`, `entry`, `with`
  and `write`; `Baseline` is one entry and loses `schema`, `read`, `parse`,
  `to_toml` and `write`; `compare::judge_file` judges against a file, and
  `run::conclude` takes the file.

## [0.1.1] - 2026-11-06

### Fixed

- The idle phase also counts the transactions that touch no relation, such
  as a loop of `SELECT 1`, which `pg_stat_database` leaves uncounted while
  their backend lives (#252). It samples the database's other client
  backends in `pg_stat_activity` every 50 ms on a connection of its own,
  and the idle transactions per second are the larger of that count and
  the `pg_stat_database` one. Reading the server's backends takes its
  role, a superuser or `pg_read_all_stats`.

## [0.1.0] - 2026-11-06

### Added

- The `ferrohealth-perf` binary, the performance gate a product's CI job
  runs against its server: `run`, with `--update`, and the offline
  `compare`, exiting 0 for a pass, 1 for a breach and 2 for a run that
  cannot be judged or a harness error (#249).
- The idle phase: the server's CPU time from `/proc/<pid>` or its cgroup
  v2 directory, and the database's transactions per second from
  `pg_stat_database`, each held to an absolute limit the product sets
  (#249).
- The load phase: the request set of `perf/plan.toml` sent open-loop at a
  fixed rate after a warm-up, latency timed from the intended instant, the
  generator's lag checked, per-group `HdrHistogram` latencies with p50, p95
  and p99 carrying 95 % rank intervals and withheld below their minimum
  samples, the steadiness check, peak memory, CPU per request and
  transactions per request (#249).
- `perf/baseline.toml`: a versioned baseline with per-metric tolerances
  and an A/A noise floor, which `--update` writes only from a run that
  passes and only lowers on the same plan (#249).
- The JSON report with the encoded histograms, written dated and as
  `latest.json` through a rename, the Markdown summary for
  `$GITHUB_STEP_SUMMARY`, and the host fingerprint with a warning when it
  differs from the baseline's (#249).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.1.1...HEAD
[0.1.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.1.0...ferrohealth-perf-v0.1.1
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-perf-v0.1.0
