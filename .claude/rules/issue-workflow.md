<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Issue workflow (the tracker loop)

**The tracker is GitHub Issues: the open issue list IS the worklist.** Issue
state is edited only via `gh`; never track work only in chat. This file is the
loop, the title, type, priority and label model, and the cadence.
Relationships between issues are native edges set with `scripts/gh/rel.sh`,
never prose in a body; this repository has no roadmap board.

## The loop

1. **Orient.** `gh issue list --state open`. Pick the pinned
   issue (pins are the current focus, max 3) or the issue the user names;
   **skip an issue shown `BLOCKED-by` an open issue** (work its blocker first)
   and prefer the next open child of a parent. Among the candidates, the
   `Priority` field decides the order (`Urgent`, `High`, `Medium`, `Low`) and
   the oldest issue goes first within a priority. Read the contract with the
   `--json` form below and its relationship graph with
   `scripts/gh/rel.sh tree <n>`.

   **Never read an issue with `gh issue view <n> --comments`.** On gh 2.102.0
   it prints nothing at all and exits 0 for an issue with no comments
   (verified 2026-10-08), so the contract comes back empty and nothing says
   so. Every reader of an issue uses:

   ```sh
   gh issue view <n> --json title,body,comments \
     --jq '.title, .body, (.comments[] | "--- comment ---", .body)'
   ```
2. **Read the contract.** The issue body opens with a plain summary (no
   heading: what and why, decisions, spec citations), followed by an
   `## Acceptance criteria` checklist and an optional `## Tasks` task list. New
   work discovered en route gets its own issue (`scripts/gh/fields.sh new`)
   that is then **linked** with `scripts/gh/rel.sh`, as a sub-issue of the
   issue it decomposes or a `blocked-by`/`blocking` dependency for real
   sequencing, never a prose "see also" deferral.
3. **Do the work.** First read the governing specification text (the
   OpenTelemetry protocol and semantic conventions, W3C Trace Context, the
   HTTP RFCs and the FerroPULSE report envelope are the oracles, vendored
   under `docs/specs/` when the crate that needs one lands). Where none
   governs a decision, say so: "no specification governs this: our own
   design". Build compiling, tested increments.
4. **Record progress on the issue.** Tick verified acceptance-criteria
   checkboxes (`gh issue edit <n>`), and post substantive status or decisions
   as comments (`gh issue comment <n>`); the issue thread is the durable
   record.
5. **Commit on a conventional-type branch** (see the branch rule below) with a
   descriptive subject; the PR body declares `Closes #<n>` so the merge into
   `main` auto-closes the issue (never close by hand when a PR carries the
   work). One `Closes` keyword closes one issue: "Closes #1, #2" closes only
   #1, so repeat the keyword per issue and verify after the merge. Arm
   auto-merge the moment the PR is open, as its own command
   (`gh pr merge <n> --auto`); the merge queue lands it once `conclusion` is
   green.
6. **Close out:** verify the acceptance criteria are met, write the close
   narrative into the PR description, and post the handoff comment on the
   issue.

## Issue titles

**An issue title is a plain description with no conventional-commit
prefix.** The native issue type already says Feature, Bug or Task, so the
title says what the issue is about: "Listener TLS, a TLS-aware healthcheck,
build info and the banner", never "feat(ferrohealth-server): listener TLS".
A pull request title keeps the conventional prefix, because it becomes the
squash commit's subject.

## Type, priority and labels

**The type, the priority and the effort of an issue are GitHub's own fields,
never labels** (owner decision 2026-10-08, the model FerroFED has used since
2026-10-02). The `FerroHEALTH` organisation defines all three, and
`scripts/gh/fields.sh` sets each by issue number, resolving every node id and
failing loud on a typo:

| Fact | Where it lives | Values | Command |
|---|---|---|---|
| Type | the native issue type of the organisation | `Bug`, `Feature`, `Task` | `scripts/gh/fields.sh type <n> bug\|feature\|task` |
| Priority | the organisation's `Priority` issue field | `Urgent`, `High`, `Medium`, `Low` | `scripts/gh/fields.sh priority <n> urgent\|high\|medium\|low` |
| Effort | the organisation's `Effort` issue field | `High`, `Medium`, `Low` | `scripts/gh/fields.sh effort <n> high\|medium\|low` |

`scripts/gh/fields.sh show <n>` prints all three with the labels and the
milestone. `scripts/gh/fields.sh new <type> <priority> <effort> <gh issue
create args…>` creates an issue that carries all three from the start, so
every issue is filed through it, never through a bare `gh issue create`.

**An issue a scheduled lane files arrives without the three, and whoever
picks it up sets them.** A scheduled workflow runs with its default
token, which may not read the organisation's issue types and issue fields:
GraphQL answers `organization: null` rather than an error. `fields.sh new`
probes for that, says so on stderr, and files the issue with the labels it
was given, so the finding is never lost. Whoever picks such an issue up sets
the type, the priority, the effort and the milestone before doing anything
else with it.

The priorities read: `Urgent` is drop everything, `High` is the current
focus, `Medium` is normal, `Low` is the backlog. Effort is the size of the
work as filed, in the filer's judgement: `Low` is one sitting (a comment, a
guard, a one-file fix), `Medium` is one pull request that touches more than
one crate or needs a test fixture, `High` is more than one pull request or a
design the orchestrator has to hold in context. It is set at filing and
re-set when the work turns out bigger; it never changes the order of the
worklist, which the priority and the age of the issue decide.

The type maps to the conventional-commit type of the branch and the commit: a
`Bug` is a `fix`, a `Feature` is a `feat`, and a `Task` names its kind with
exactly ONE work-kind label. `scripts/gh/labels.sh` retires the six labels
that once carried type and priority (`bug`, `enhancement`, `P0` to `P3`)
wherever they appear.

Labels carry what the platform has no field for. Bootstrap them with
`scripts/gh/labels.sh`:

- **Work kind, on a `Task` only, exactly ONE:** `documentation` (docs),
  `chore`, `refactor`, `perf`, `test`, `ci`. A `Bug` and a `Feature` carry
  none, because the type already names the commit type.
- **Crate, on every issue, at least ONE:** `secret`, `config`, `telemetry`,
  `health`, `server`, `usage-report`, naming the `ferrohealth-*` crate the
  work lands in.
- **Specification, where one applies:** `spec:OpenTelemetry` (the
  OpenTelemetry specification and W3C Trace Context) and `spec:TLS` (TLS 1.3
  and certificate handling). Add one when a crate starts to implement
  another specification; keep the set small and meaningful.
- **Workflow:** `security`, `dependencies` (Dependabot), `blocked-upstream`
  (waits on an upstream specification or tool release), and the pull-request
  escape hatch `no-changelog` (the change has no effect a product sees).

Every new issue is filed with `scripts/gh/fields.sh new` and carries its
crate label, a specification label where one applies, and, for a Task,
exactly one work-kind label. `scripts/gh/labels.sh` bootstraps exactly this
set, with its colours and descriptions, and retires `accessibility` and the
six labels that once carried type and priority.

The organisation also defines `Start date` and `Target date` issue fields;
this repository leaves both empty, because the milestone is the release spine
and its due date is the target.

## Milestones = releases

A milestone is a delivery promise for one crate, titled exactly as the
release tag it ends in: `ferrohealth-<crate>-vX.Y.Z`. A release is cut when
its milestone reaches zero open issues (or the owner calls the cut and moves
the stragglers to the next milestone). Every en-route issue goes in the
CURRENT milestone of its crate, never the next. A new milestone gets a due
date.

**The release workflow enforces both ends.** Its `plan` job refuses a tag
when no milestone is titled exactly as the tag or that milestone still holds
an open issue or pull request, and names each one. Its last job closes the
milestone once every leg has succeeded, crates.io included, so a published
release never leaves its milestone open. Both call
`scripts/release/milestone.sh` (`docs/release.md`). After a manual or
recovery release, check that the milestone is closed.

## The fix-first cadence

**An issue filed while working a unit is FIXED before the next unit starts.**
Section findings, verification-pass findings, guard gaps, and en-route defects:
filing the issue is the record, never permission to move on. Tractable findings
are fixed in the same branch; genuinely separable work becomes a linked issue
that is closed before the current program advances.

## Branches use conventional types

`<type>/<kebab-case-slug>` with `type` in `feat`, `fix`, `chore`, `docs`,
`refactor`, `perf`, `test`, `ci`, `build`, `release` (the Conventional Commits
type set): for example `feat/mapping-loader`, `fix/its-rest-status-mapping`.
Pick the type by the dominant change; an issue's branch is normally
`feat/<issue-slug>` mirroring the issue's type (a `Feature`) or, for a
`Task`, its work-kind label. Never force-push `main`.

## Durable versus session-scoped

Issues and git survive `/clear` and `/compact`; the built-in todo tool is
session-scoped, so the tracker is the durable layer. Record progress (tick
checkboxes, comment, open issues for new work) and commit before ending a
session.

## Never add AI or Claude attribution

Commit messages, PR text, issue and comment bodies describe only the change
itself: no `Co-Authored-By: Claude`, no "Generated with", no bot trailer or
footer, ever. This is an absolute rule with no exceptions.
