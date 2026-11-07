<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-perf

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-perf-vX.Y.Z`.

## [Unreleased]

## [0.4.1] - 2026-11-07

### Fixed

- A host shape keys its memory by the host's `MemTotal` rounded to the
  nearest GiB (#269). Runners of one size report a `MemTotal` a few KiB
  apart, so 0.4.0 split one runner size into several shapes and most fresh
  runners had no entry. An entry 0.4.0 wrote with an exact byte count is
  found by every host of its rounded size, and the message names the size
  in GiB.

## [0.4.0] - 2026-11-07

### Changed

- Breaking: a load breach is confirmed on fresh runs before it fails the
  gate (#265). When a load metric breaches its entry, `run` measures the
  load phase again, each time after its own warm-up, up to two more times,
  and fails the metric only when it breaches in most of the runs. A one-off
  stall or a noisy runner clears, and a real regression fails in every run.
  The JSON report moves to schema 3 with the further phases under
  `confirmations`, each check carries `runs` and `breached`, and its rule
  names in how many runs it breached; `compare` still reads schemas 1 and 2.
  No tolerance moves.
- Breaking: a baseline entry is keyed by the host shape, the CPU model and
  the total memory of the fingerprint together, so hosts of one CPU model
  and another memory size never share an entry (#265; one EPYC 9V45 ran
  about 35 % slower on another memory size). An entry committed by 0.3.0 is
  found only by a host of the memory size it recorded, and a host of
  another size exits 2 naming its shape. In the library `Baselines::entry`
  takes a `Shape`, `Baseline::model` is replaced by `Baseline::shape`, and
  `model_name` by the `Display` of `Shape`.

### Added

- A load phase where every route's p99 is over `[load] stall_ratio` (10 by
  default) times its p50 at once marks a stalled runner: it is reported,
  never judged, and the run exits 2; a stalled confirmation is not counted
  (#265).
- `ferrohealth-perf merge --baseline <file> <job>...` merges the baselines
  several `--update` jobs wrote into the committed one, per host shape: the
  median over the jobs and the committed entry, and as the noise floor the
  larger of their spread and every job's own floor, so the floor holds the
  spread between runners and not only within one (#265).
- A load phase presents the bindings of a sender-constrained token (#264):
  `[load] client_identity_file`, a certificate chain and key presented on
  every TLS connection (RFC 8705 §3), with `[load] trust_roots_file` for an
  `https` target, and per request `dpop_key_file`, a P-256 key that signs a
  fresh `DPoP` proof for every send (RFC 9449 §4.2), with `ath` over the
  request's bearer token and the token under the `DPoP` scheme. A credential
  file that cannot be read or parsed refuses the plan. In the library,
  `load::drive` takes the `credentials::Credentials` it presents.

## [0.3.0] - 2026-11-07

### Changed

- Breaking: `--update` measures the noise floor across runs (#261). It runs
  `--runs` load phases (5 by default, at least 2), each after its own
  warm-up, writes each metric as their median, and writes as its noise
  floor how far the worst phase strayed from that median. The ratchet
  tightens a metric only when a new update's median is better, never from
  one lucky phase. A metric still fails past the larger of its tolerance
  and that floor, so no tolerance is loosened: a p99 tail that moves within
  what the runs showed passes, and a regression past it fails. The baseline
  file moves to schema 3. A schema-2 or older entry keeps its values marked
  `remeasure = true`; it judges no load phase (the run exits 2 asking for
  an update, the idle limits still judging), and the next `--update` on its
  model measures it afresh. Re-measure every committed entry after the
  upgrade. The JSON report moves to schema 2, with `calibration` a list of
  the update's further phases, and `compare` still reads a schema-1 report.
  In the library, `baseline::noise_floor` is replaced by `baseline::across`,
  `Baseline` gains `remeasure`, `Change` gains `Remeasured`, and
  `RunOptions` gains `runs`.

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

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.4.1...HEAD
[0.4.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.4.0...ferrohealth-perf-v0.4.1
[0.4.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.3.0...ferrohealth-perf-v0.4.0
[0.3.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.2.0...ferrohealth-perf-v0.3.0
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.1.1...ferrohealth-perf-v0.2.0
[0.1.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-perf-v0.1.0...ferrohealth-perf-v0.1.1
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-perf-v0.1.0
