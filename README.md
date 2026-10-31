<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->
<div align="center">

<img src="assets/brand/ferrosys-lockup-auto.svg" alt="FerroSYS" width="290">

**The system plumbing every FerroHEALTH server shares, as Rust libraries: secrets, configuration, logging and telemetry, health checks and the HTTP server.**

[Family site](https://ferrohealth.eu/) ·
[What ships today](#what-ships-today) ·
[Milestones](https://github.com/FerroHEALTH/FerroSYS/milestones) ·
[Licensing](https://ferrohealth.eu/docs/licensing.html)

**Project**<br>
[![License: BUSL-1.1](https://img.shields.io/badge/License-BUSL--1.1-blue)](LICENSE)
[![Last commit](https://img.shields.io/github/last-commit/FerroHEALTH/FerroSYS?logo=github&label=last%20commit)](https://github.com/FerroHEALTH/FerroSYS/commits/main)
[![Open issues](https://img.shields.io/github/issues/FerroHEALTH/FerroSYS?logo=github&label=issues)](https://github.com/FerroHEALTH/FerroSYS/issues)
[![Family](https://img.shields.io/badge/family-FerroHEALTH-3F6070?logo=rust&logoColor=white)](https://ferrohealth.eu/)

**Quality and security**<br>
[![Security policy](https://img.shields.io/badge/security-policy-brightgreen?logo=github)](SECURITY.md)

</div>

## What FerroSYS does

Every FerroHEALTH server has to do the same handful of things before it can
do its real job. It writes a log that a person or a log pipeline can read. It
sends traces and measurements to the monitoring system a hospital already
runs. It tells the container platform whether it is alive and ready for
work. It stops cleanly when asked to, without dropping a request halfway. It
reads its settings from a file and the environment, and keeps passwords out
of every log line.

Today each server has its own copy of that code, and the copies have drifted
apart: one setting has a different name in two products, and a log line
looks different from one server to the next. FerroSYS is where that code
moves, so every server behaves the same way and an operator learns it once.

The libraries touch no clinical content. They carry how a server runs,
never what it stores or translates.

### Who it is for

- **Hospital IT and operations teams**, who run several FerroHEALTH servers
  and want one log format, one set of health checks and one way to configure
  each of them.
- **Developers of the FerroHEALTH servers**, who take these libraries
  instead of writing the plumbing again.

### Why you can trust what it claims

The source is public, and the design is recorded on the
[tracker](https://github.com/FerroHEALTH/FerroSYS/issues) of this
repository, where you can follow every decision. There is no paid edition
with extra features. This README describes only what exists; what is planned
is on the milestones.

## Part of FerroHEALTH

FerroHEALTH is five servers, each its own product, that reach each other
only over published standards: FerroEHR stores the record, FerroBRIDGE
translates it to FHIR and the OMOP research model, FerroFED connects records
across organisations and borders, FerroPIX matches patient identities, and
FerroTERM serves the code systems (the shared lists of medical terms). No
server is built into another.

Work that every server needs lives in shared libraries, so it is written
once: FerroAUTH for sign-in and access control, FerroAUDIT for the audit
trail, FerroUI for the web consoles, and FerroSYS for the system plumbing
(logging, OpenTelemetry, health checks, the HTTP server stack and
configuration). The family rule is that nothing is hand-written that a
shared library provides; when a library lacks something, it is added there
and released.

FerroSYS is a repository of shared crates, like FerroAUTH and FerroAUDIT:
the crates run inside each server and take no call of their own. It
distributes no configuration, deploys nothing and runs no service.

## What ships today

Nothing yet: FerroSYS has no released crate and no code. The repository
holds the licence, the brand, the tracker tooling and the plan in
[`docs/architecture.md`](docs/architecture.md). What is built, and in which
order, is on the
[milestones](https://github.com/FerroHEALTH/FerroSYS/milestones).

## Learn more

- The [FerroHEALTH family site](https://ferrohealth.eu/) and the [family book](https://ferrohealth.eu/docs/)
- The [architecture](docs/architecture.md), the [milestones](https://github.com/FerroHEALTH/FerroSYS/milestones) and the [tracker](https://github.com/FerroHEALTH/FerroSYS/issues)
- [Security policy](SECURITY.md)
- [Contributing](CONTRIBUTING.md) and [maintainers](MAINTAINERS.md)
- [Brand](assets/brand/README.md)

## Licence

Business Source License 1.1 ([LICENSE](LICENSE)), with Apache License 2.0 as
the Change License four years after each version; the
[licensing page](https://ferrohealth.eu/docs/licensing.html) says when you
need a commercial licence from Cadasto B.V.
