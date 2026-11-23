---
name: ehds-above-openehr
description: where the EHDS Regulation (EU) 2025/327 conflicts with an openEHR or other standard's text, the Regulation wins; never undo EHDS work to turn a conformance row green (owner, 2026-11-09)
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

Where Regulation (EU) 2025/327 (EHDS) conflicts with the text of openEHR or
another standard, the Regulation wins. Owner, 2026-11-09: "we are not
deleting all our hard work to make our code EHDS ready. EHDS is above openEHR
specs."

**Why:** every FerroHEALTH product is built for the EU EHR market and must
meet the Regulation. The specification stays the oracle everywhere the
Regulation is silent.

**How to apply:** never offer "revert the EHDS behaviour" as an option, in
any product. A conflict is a recorded deviation with its legal ground
(article and annex point) and an upstream report; a conformance suite records
it as a regulatory deviation, never as a failure to fix. Related:
[[decide-dont-ask]].
