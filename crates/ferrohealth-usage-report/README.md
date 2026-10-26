<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-usage-report

The installation report of the FerroHEALTH products to FerroPULSE, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). Every product sends the
same two reports on FerroPULSE's report envelope v2, and supplies only its
own `status`.

## What it does

- `section::UsageReportSection` is the `[usage_report]` section: the off
  switch, the collector (`https`, or `http` to loopback, with no userinfo),
  the state directory and the deployment kind. A CI run sends nothing.
- `state::StateDir` creates the instance id once under `state_dir` and reads
  it back on every start; a damaged id is refused rather than replaced.
- `window::Window` holds the latency histogram and the server errors per
  request group, and the product's counters. The request log fills it with
  route templates, never paths.
- `envelope` holds the closed types of envelope v2.
- `reporter::Reporter` sends one `start` report at each start (at most one
  per ten minutes) and one `daily` report a day. A refusal is logged with
  the collector's status. A daily report that does not arrive keeps its
  counts for the next try.
- A product implements `reporter::ProductSection` for its `status`.

## Use

```toml
[dependencies]
ferrohealth-usage-report = "=0.1.0"
```

Pin the version exactly, as every FerroHEALTH product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
