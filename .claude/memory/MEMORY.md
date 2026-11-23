<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Memory index

- [Family architecture](family-architecture.md): the FerroHEALTH family
  is five separate servers that reach each other only over published
  protocols, plus shared crates for cross-cutting work; nothing is
  hand-written that a shared crate or another product provides; owner
  2026-10-25
- [What FerroSYS is](ferrosys.md): a repository of shared crates like
  FerroAUDIT, with no service of its own; six `ferrohealth-*`
  crates, per-crate tags, lockstep; owner 2026-10-26
- [Memory lives in the repo](memory-lives-in-repo.md): every learning is a
  tracked file here, never a per-user note
- [Licence: BUSL 1.1](license-busl.md): the FerroHEALTH terms, Cadasto B.V.
  the Licensor, Apache 2.0 four years after each version

## Family rules (carried in every FerroHEALTH repository, 2026-11-09)

- [FerroHEALTH not in production](ferrohealth-not-in-production.md): no product runs in production, so a wrong design is refactored or rewritten outright with no compatibility layer; owner 2026-11-19
- [New design, no legacy](new-design-no-legacy.md): a breaking release builds the new design in full and keeps nothing of the old; never relay a worker's kept-for-compatibility choice as a question; owner 2026-11-14
- [Findings drive redesign](findings-drive-redesign.md): a scenario or test the design cannot handle is a design finding, never worked around in a fixture or probe; owner 2026-11-14
- [Verify old issues first](verify-before-implementing-old-issues.md): check an older issue against main and the current spec before building it; close what is done or obsolete with evidence; owner 2026-11-16
- [Orchestrator files cross-repo gaps](orchestrator-files-cross-repo-gaps.md): the orchestrator files every gap a worker finds in another family repo, without asking the owner; owner 2026-11-10
- [Backlog fix comments](backlog-fix-comments.md): when a fix for a Cadasto/FerroHEALTH-backlog issue merges, comment there and set it to In review; owner 2026-11-17
- [Efficiency first](efficiency-first-rust.md): stable, fast, low on storage, CPU and RAM; no needless clones, allocations or forwarding layers; owner 2026-11-16
- [Spec over test suites](spec-over-test-suites.md): external conformance suites are regression signals, never the oracle; owner 2026-11-16
- [Owner voice for outward text](owner-voice-outward-text.md): text the owner sends as themselves is plain and short, few headings or bullets; owner 2026-11-14
- [Owner work style](owner-work-style.md): research first and evidence-based, foundational decisions confirmed before code, defer nothing
- [EHDS above openEHR](ehds-above-openehr.md): where Regulation (EU) 2025/327 conflicts with a standard's text, the Regulation wins; owner 2026-11-09
- [Terminology via the server](terminology-via-server.md): every production value map goes through a FHIR terminology server; embedded maps are for tests only; owner 2026-11-09
- [Release closes its milestone](release-closes-milestone.md): release.yml refuses a tag whose milestone is missing or open and closes it after every leg; check after a manual release
- [No compatibility shims](no-compat-shims.md): refactors break APIs freely and leave no deprecated item, alias, re-export or stub; a removal refusal lives ONE release, then is deleted fully; no legacy support ever; owner 2026-11-01, 2026-11-09
- [No half solutions](no-half-solutions.md): build what the issue asks the proper way and validate the need against its consumer first; a breaking change moves the minor version, never shape a design to dodge semver; owner 2026-11-08
- [Workers on Opus only](workers-on-opus-only.md): every subagent is model opus; never Fable, Sonnet or Haiku as a worker; owner 2026-10-30
- [Ten worker slots](worker-cap-ten.md): up to 10 implementation workers at once across the family since 2026-11-03, replacing the cap of 5
- [No workflows for workers](no-workflows-for-workers.md): start workers with the Agent tool only, since a workflow agent cannot be messaged; owner 2026-11-01
- [Worker scratch subfolders](worker-scratch-subfolders.md): scratch files under the worker's own scratchpad subfolder, never the root; check a file is yours before --body-file and re-read the PR body; 2026-10-20
- [Worker wait loops](worker-wait-loops.md): no self-matching pgrep waits, one wait per output, push a finished branch, stop a process by PID and never pkill; 2026-10-16
- [Workers never idle on the merge queue](workers-never-idle-on-merge-queue.md): start the next independent issue in a fresh worktree while a PR waits in the queue; owner 2026-11-03
- [Workers own every open PR](workers-own-every-open-pr.md): drive every PR you opened to merge; the orchestrator sweeps every open PR after each merge, red main first; owner 2026-11-01, 2026-11-03
- [PR backlog discipline](pr-backlog-discipline.md): several open PRs are fine if each merges cleanly; disjoint files, no stacked PRs, every PR tracked to its merge; owner 2026-10-17
- [Worktrees in the repo](worktrees-in-repo.md): every worktree at <repo>/.claude/worktrees/<name>, never a sibling folder or the scratchpad; owner 2026-10-21
- [Remove worktrees after merge](remove-worktrees-after-merge.md): remove each worktree, its branches and its target/ once its PR merges and its worker has handed back; owner 2026-10-15, 2026-10-19
- [Admin bypass stays](admin-bypass-stays.md): the org-admin bypass on main stays in every family repo; nobody merges through it; owner 2026-10-21
- [Decide, do not ask](decide-dont-ask.md): make tracker, design and release calls yourself and act; ask the owner only when a tool is refused; owner 2026-11-03
- [Autonomous loop](autonomous-loop.md): never stop and never wait idle; keep working the issues while PRs drain; owner 2026-10-17
- [Always a next milestone](next-milestone-always.md): every repo keeps a milestone with a due date after the active one; owner 2026-11-03
- [One milestone at a time](one-milestone-at-a-time.md): every worker on the current milestone until it is cut; en-route issues join it and are fixed first; owner 2026-10-19
- [Shared-crate blockers](shared-crate-blockers.md): every filed issue gets a milestone; a shared-crate defect goes in the owner repo's current milestone with a worker at once; no consumer workaround; owner 2026-11-07
- [Release cuts run from the session](release-tag-and-approve-autonomously.md): merge the release PR, sign and push the tag and approve the crates-io gate yourself; owner 2026-09-13 to 2026-11-03
- [Family toolchain](family-toolchain.md): newest stable Rust as toolchain, the toolchain minus one as MSRV; never lower an MSRV; owner 2026-10-21
- [Signed commits and the merge queue](merge-queue-signed.md): main takes only signed commits through the merge queue; no commit-message line starts with "-" or a git header word; 2026-11-09
- [PR auto-merge](pr-auto-merge.md): arm gh pr merge <n> --auto as its own command the moment a PR opens; no --squash, --delete-branch or --admin; owner 2026-09-04
- [Subagent reports go to a file](subagent-reports-to-file.md): a long agent report is written to the agent's scratch subfolder and read from there; 2026-09-12
- [Sibling projects](sibling-projects.md): the other family repositories are read-only prior art from here; a change there is made in its own checkout and PR
- [Lean comments, no history](lean-comments-no-history.md): comments rare and short, describing only the current code; a changed comment is rewritten whole, never extended with the old way; owner hard rule 2026-11-23
- [Family memories in every repo](family-memories-every-repo.md): a family-wide owner rule goes into every FerroHEALTH repo's `.claude/memory` through a merged PR; owner 2026-11-23
