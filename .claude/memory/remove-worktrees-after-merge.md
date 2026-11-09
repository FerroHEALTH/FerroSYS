---
name: remove-worktrees-after-merge
description: Remove every worktree, its local branches and its build folder as soon as its PR merges, in every FerroHEALTH repository; never remove one whose worker is still running (owner 2026-10-15, 2026-10-19, 2026-11-01)
metadata:
  type: feedback
---

<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

Remove a worktree under `.claude/worktrees/` and its local branches (`<branch>`, `worktree-agent-*`) as soon as the PR carrying its work is merged, and run `git fetch --prune`. A worktree whose work was discarded or superseded goes too.

**Why:** the owner found leftover worktrees and asked (2026-10-15) that they "never hang there"; the disk stood at 1.1 TB on 2026-10-19 ("keep cleaning the cargo and things"); a sweep on 2026-11-01 found about 60 stale worktrees across the family ("always remove old worktrees, never forget").

**How to apply:** on each merge: `git pull --ff-only` in the main checkout, `cargo clean` in the worktree if it built anything, `git worktree remove .claude/worktrees/<name>`, `git branch -D <branch>` (a squash-merged branch fails `-d`), `git fetch --prune`. Before removing one, check it holds no uncommitted or unpushed work, that its PR is merged, and that its worker has handed back; never loop over `.claude/worktrees/*`, since a loop cannot tell a live worktree from a finished one. A dirty worktree is reported, never deleted. Every worker brief ends with "remove your worktree once your PR has merged". Related: [[worktrees-in-repo]].
