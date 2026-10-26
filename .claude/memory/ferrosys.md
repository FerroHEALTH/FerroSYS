---
name: ferrosys
description: "FerroSYS is a repository of shared crates like FerroAUTH and FerroAUDIT: no configuration distribution, no deployment, no central service; six ferrohealth-* crates with per-crate tags in lockstep; owner 2026-10-26"
metadata:
  type: project
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

FerroSYS holds the system plumbing every FerroHEALTH server writes for
itself today, as shared Rust crates: secrets and redaction, configuration
loading, the logging console, the request log and OpenTelemetry, the health
family, the HTTP server stack with the shutdown drain, and the usage report
to FerroPULSE. The crates run inside each product and take no call.

It distributes no configuration, deploys nothing, and no central service is
planned. The six crates (`ferrohealth-secret`,
`-config`, `-telemetry`, `-health`, `-server`, `-usage-report`), their
policies and the migration order are recorded in `docs/architecture.md`.

**Why:** the owner's rulings on 2026-10-26: the five products each carry a
diverged copy of the same plumbing, which breaks the family rule that every
shared concern lives in one crate; FerroSYS is the same kind of repository as
FerroAUTH and FerroAUDIT, and its lockup is drawn in the parent's steel.

**How to apply:** never describe FerroSYS as a service that servers report to
or are configured from, and never promise one. Each crate has its own version line and `<crate>-vX.Y.Z` tag, and
every release rolls out to every product at once. No crate is built before
its crates.io name is reserved, which needs the owner.
