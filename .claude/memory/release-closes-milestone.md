---
name: release-closes-milestone
description: the release workflow refuses a tag whose milestone is missing or not empty, and closes the milestone after every leg succeeds; after a manual or recovery release, check it is closed (owner, 2026-10-19)
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

A published release closes its milestone: `release.yml` refuses a `vX.Y.Z`
tag in `plan` when the milestone titled exactly as the tag is missing or
holds an open item, and its last job closes that milestone once every leg
has succeeded (`scripts/release/milestone.sh`).

**Why:** the owner wants a published release with an open milestone to be
impossible in every product.

**How to apply:** cut a release when its milestone holds no open item. After
a manual or recovery release, check the milestone is closed, and run
`GITHUB_REPOSITORY=FerroHEALTH/<repo> bash scripts/release/milestone.sh close vX.Y.Z`
when it is not.
