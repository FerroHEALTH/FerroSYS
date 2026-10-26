---
name: ferrosys
description: "FerroSYS is the family's shared system crates (logging, OpenTelemetry, health, server plumbing, configuration) first, and the central log and console service later; owner direction 2026-10-26"
metadata:
  type: project
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

FerroSYS holds the system plumbing every FerroHEALTH server writes for
itself today, as shared Rust crates: the logging console and the request
log, OpenTelemetry export, the health family and the container healthcheck,
the HTTP middleware stack and the shutdown drain, and configuration loading
with `_file` secrets. Every product then uses the same code and behaves the
same way.

The central log and console service (the central half of the logging
component in FerroHEALTH/FerroFED#825) lives in this repository too, and is
built after the crates.

**Why:** the owner's direction on 2026-10-26: the five products each carry a
diverged copy of the same plumbing, which breaks the family rule that every
shared concern lives in one crate.

**How to apply:** describe FerroSYS this way in every document. The crate
layout and the migration order are open until the owner decides them; do
not scaffold a crate before its issue and milestone exist. The repository
description and the family site still call FerroSYS a control plane; that
wording is the owner's to align.
