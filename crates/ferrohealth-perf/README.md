<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-perf

The performance gate of the FerroHEALTH servers, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). A product's CI job
starts its server and its PostgreSQL, then runs `ferrohealth-perf` against
them. The run fails when the server is busy at rest, or slower, heavier or
busier under load than the baseline the product committed.

## What it measures

1. **Idle phase** (60 s by default, no requests). The server's CPU time, as
   a percentage of one core, and the database's transactions per second
   are held to absolute limits, 2 % and 5/s by default. The memory at the
   start and the end is reported. The transactions per second are the
   larger of two counts over the window, each of which can miss
   transactions but never counts one that did not happen:
   - the committed and rolled-back transactions of `pg_stat_database`
     (`xact_commit` plus `xact_rollback`). PostgreSQL reports a backend's
     transactions together with its other pending statistics, so a loop
     that polls a table, such as an outbox sender, is counted within a
     second, and a loop of transactions that touch no relation, such as a
     bare `SELECT 1`, is not counted while its backend lives;
   - the transactions the database's other client backends are seen to
     start in `pg_stat_activity` (`xact_start` and `query_start`), sampled
     every 50 ms. This count includes the transactions that touch no
     relation, at most one per backend per sample, so a `SELECT 1` loop
     shows as about 20/s per backend. Reading the server's backends takes
     its role, a superuser or `pg_read_all_stats`; for any other role
     PostgreSQL hides their times, and only the first count remains.
2. **Load phase.** The request set is sent open-loop at a fixed rate, after
   a warm-up. Each latency is timed from the instant the request was due,
   so a stalled server raises the latency of every request it held back. A
   run whose own generator lagged is refused. It records per request group
   the p50, p95 and p99 with their 95 % rank intervals (a quantile with too
   few samples is withheld: p99 needs 100), and overall the throughput,
   the peak memory, the CPU time per request and the transactions per
   request. The second half of the window must match the first within 0.8
   to 1.25 on p50 and p95, or the run is not steady and cannot be judged.

CPU and memory come from `/proc/<pid>` (`--pid`) or from a cgroup v2
directory (`--cgroup`, or `--container` with a container id). The Docker
socket is never asked. A host without `/proc` reports both as unavailable.

## Use

```sh
cargo install ferrohealth-perf --version =0.2.0 --locked
```

Pin the version exactly, as every FerroHEALTH product does.

```sh
ferrohealth-perf run --pid "$SERVER_PID" --base-url http://127.0.0.1:8080 \
  --database-url "$DATABASE_URL"
ferrohealth-perf run ... --update      # write or lower this CPU model's entry
ferrohealth-perf compare --report target/perf/latest.json
```

| Flag | Default | Meaning |
|---|---|---|
| `--plan` | `perf/plan.toml` | the limits, the load and the request set |
| `--baseline` | `perf/baseline.toml` | the committed baseline, one entry per CPU model |
| `--pid`, `--cgroup`, `--container` | one is required | the server's process, cgroup or container |
| `--cgroup-root` | `/sys/fs/cgroup` | where `--container` looks |
| `--base-url` | required | the server's base URL |
| `--database-url` | `FERROHEALTH_PERF_DATABASE_URL` | the database whose transactions are counted |
| `--out-dir` | `target/perf` | where the JSON reports go |
| `--summary` | none | a file the Markdown summary is also written to |
| `--update` | off | write the entry of the runner's CPU model from a run that passes |
| `--runs` | `5` | the load phases `--update` measures, at least 2 |

Exit codes: **0** the run passed, **1** a limit or tolerance was breached
or a request was answered other than expected, **2** the run cannot be
judged (no baseline, no entry for the runner's CPU model, a baseline of
another plan, a metric the baseline holds and the run lacks, a lagging
generator, an unsteady window) or a harness error. The idle limits judge
every run, so a busy server fails with 1 even where the load phase cannot
be judged. Every credential in a URL is redacted in the output.

## The plan, `perf/plan.toml`

```toml
[idle]
duration_secs = 60                 # the idle window
max_cpu_percent = 2.0              # of one core
max_transactions_per_second = 5.0
settle_secs = 11                   # wait before it, so earlier statistics land

[load]
rate = 50.0                        # requests per second, open loop
warmup_secs = 10
duration_secs = 60                 # the measured window
max_in_flight = 256
saturation_ms = 5000               # a wait this long for a slot fails the run
timeout_ms = 10000
max_lag_ms = 10                    # the generator's lag p99 limit
sample_ms = 100                    # how often memory is sampled
settle_secs = 11                   # wait after it, before the transaction count

[[requests]]
group = "metadata"
method = "GET"
path = "/fhir/metadata"
status = 200

[[requests]]
group = "create"
method = "POST"
path = "/fhir/Patient"
status = 201
body = '{"resourceType":"Patient"}'
headers = { content-type = "application/fhir+json" }
```

Every key but `requests` has the default shown; an unknown key is refused.
Arrival `i` sends request `i` modulo the set, so repeat an entry to weight
it.

## The baseline, `perf/baseline.toml`

Shared CI runners mix CPU generations: on GitHub's hosted ubuntu runners
the same commit measured 25 to 45 % slower on an AMD EPYC 7763 than on an
EPYC 9V45. A latency or a CPU time is comparable only on a like CPU, so the
file holds one `[[baseline]]` entry per CPU model, keyed by the
`cpu_model` of the host fingerprint (`model name` in `/proc/cpuinfo`). A
run is judged against the entry for its own model. A model with no entry
cannot be judged: the run exits 2, naming the model and the models the file
holds, and the idle limits still judge it.

`--update` writes the entry of the runner's model from `--runs` load phases
(5 by default), each after its own warm-up. The first is judged like any
run, and only a run that passes is written. Each metric of the entry is the
median of the phases, and its noise floor is how far the worst phase strayed
from that median. A p99 tail moves from run to run far more than a p50: on
FerroPIX one commit measured a p99 30 to 96 % apart on the same CPU model,
and a floor taken across runs is what lets the gate tell that noise from a
regression. Every other entry is left as it was. On the same plan each
metric of the entry moves only to the better of the entry and the new
median, so the entry is a ratchet that one lucky phase cannot tighten; a
raise is a hand edit in a change that says why. A plan with another rate,
window or request set writes the entry afresh. An update takes about
`--runs` times the load window, so five phases of a 60-second window with a
10-second warm-up add about six minutes to the job.

```toml
schema = 3

[[baseline]]
tool = "0.3.0"
written = "2026-11-07T10:00:00Z"

[baseline.plan]          # the plan it was measured with
rate = 50.0
warmup_secs = 10
duration_secs = 60
requests = 2
groups = ["create", "metadata"]

[baseline.host]          # the host fingerprint; cpu_model keys the entry
arch = "x86_64"
os = "linux"
available_parallelism = 4
cpu_model = "AMD EPYC 7763 64-Core Processor"

[baseline.tolerance]     # the product's, kept by every update
latency = 0.25
throughput = 0.2
memory = 0.2
cpu = 0.25
transactions = 0.1

[baseline.metrics]
throughput_rps = 49.9
peak_memory_bytes = 104857600.0
cpu_per_request_ms = 0.8
transactions_per_request = 2.0
"metadata.p50_ms" = 1.9
"metadata.p95_ms" = 4.1
"metadata.p99_ms" = 7.3

[baseline.noise]         # the floor across the update's phases, a ratio per metric
"metadata.p95_ms" = 1.31

[[baseline]]             # the next model, with its own values
# ...
```

A metric fails when it is worse than its entry by more than the larger of
its tolerance and its noise floor. The `schema` key lets a later version
migrate an older file before it reads it: a schema-1 file, the single
baseline of 0.1, is read as one entry of the model its host names. An entry
of schema 2 or older was measured within one run, so it is kept marked
`remeasure = true`: it judges no load phase, the run exits 2 asking for an
update, and the next `--update` on that model measures it afresh.

## The report

Each run writes `report-<UTC time>.json` into `--out-dir` and replaces
`latest.json` there through a rename. The report holds every measurement,
the host fingerprint and each group's latency histograms in the
`HdrHistogram` V2 encoding, base64-wrapped, so `compare` re-derives every
quantile from the report and contacts no server. The Markdown summary goes
to stdout, to `--summary`, and is appended to `$GITHUB_STEP_SUMMARY` when
that is set.

## Wiring it into CI

```yaml
perf:
  runs-on: ubuntu-24.04
  services:
    postgres:
      image: postgres:18.6@sha256:<digest>
      env:
        POSTGRES_HOST_AUTH_METHOD: trust
      ports: ["5432:5432"]
  env:
    FERROHEALTH_PERF_DATABASE_URL: postgres://postgres@127.0.0.1:5432/postgres
  steps:
    - uses: actions/checkout@<sha>
    - run: cargo install ferrohealth-perf --version =0.2.0 --locked
    - run: cargo build --release --locked
    - name: Start the server
      run: |
        ./target/release/<server> serve &
        echo "SERVER_PID=$!" >> "$GITHUB_ENV"
        timeout 60 sh -c 'until curl -fsS http://127.0.0.1:8080/health/ready; do sleep 1; done'
    - name: Gate the performance
      run: ferrohealth-perf run --pid "$SERVER_PID" --base-url http://127.0.0.1:8080
    - uses: actions/upload-artifact@<sha>
      if: always()
      with:
        name: perf-report
        path: |
          target/perf/
          perf/baseline.toml
```

For a server in a container, pass `--container "$(docker inspect -f
'{{.Id}}' <name>)"` in place of `--pid`. Commit `perf/plan.toml` first. To
write or lower an entry, run the job with `--update` on the runner pool the
gate runs on, take `perf/baseline.toml` from the uploaded artifact, and
commit it; the file holds the committed entries and the run's own. Commit
an entry for every CPU model the pool serves: re-run the job until each
model has its entry. A pool that later brings a new model fails with exit 2 and
names it, and that model's entry is added the same way.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`. The design
of the load phase and the comparison follows Veredictum (Apache License
2.0); see the repository's `NOTICE`.
