---
name: owner-work-style
description: how the owner wants work done in every FerroHEALTH repository: research first, evidence-based, from first principles, foundational decisions confirmed before code
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

The owner decides foundational architecture from cited research (academic
papers included), not convention, and is skeptical of the way others built
this kind of software before.

**Why:** the foundation is the part that is hardest to change later.

**How to apply:**

- For a foundational choice, research properly and put the evidence and a
  recommendation in front of the owner before building.
- Do not scaffold code while the design is open; build once the direction is
  confirmed.
- Test the owner's design intuitions against evidence and reconcile, never
  dismiss.
- Pure Rust, memory-safe, lightweight, single binary: the standing
  constraints across the family.
- Defer nothing; a proper rewrite is welcome ([[no-half-solutions]]).
- When the owner asks to pause at a boundary, finish the work up to it
  (merges, cleanup, the report) and launch nothing past it until the owner
  writes again.
