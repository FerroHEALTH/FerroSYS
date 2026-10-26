---
name: pr-licence-box
description: "Tick the pull request template's contribution-licence checkbox when opening a pull request; the owner's standing consent"
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

Every pull request body ticks "I accept the terms in CONTRIBUTING.md §
Licensing of contributions" before `gh pr create`.

**Why:** the owner's standing acceptance as the contributor, carried over from
FerroBRIDGE (2026-10-08); the `contribution-licence` check reads the box.

**How to apply:** build the body from `.github/PULL_REQUEST_TEMPLATE.md` with
the licence box ticked, open the pull request, then arm auto-merge.
