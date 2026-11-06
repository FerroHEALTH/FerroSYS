<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-perf

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-perf-vX.Y.Z`.

## [Unreleased]

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
