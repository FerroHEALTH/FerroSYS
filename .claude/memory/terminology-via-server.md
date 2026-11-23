---
name: terminology-via-server
description: code translation in production always goes through a FHIR terminology server (FerroTERM is the reference); an embedded or offline value map is for tests only (owner, 2026-11-09)
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

A value map (code A to code B, an HL7 v2 table, a local code to SNOMED CT or
LOINC) goes only through a FHIR terminology server (`$translate`, `$lookup`,
`$validate-code`, through `ferrobridge-term`), never an embedded, offline or
hand-written copy. A structure map (which field goes where) is mapping logic,
generated at build time from its vendored, pinned source. Offline means
without a CDR, never without terminology. Owner, 2026-11-09: "without
terminology server makes absolutely no sense".

**Why:** code systems, value sets and ConceptMaps live in the terminology
server ([[family-architecture]]); a second copy drifts from it.

**How to apply:** tests use terminology stubs or a FerroTERM container.
Never propose "no terminology server" as a product posture.
