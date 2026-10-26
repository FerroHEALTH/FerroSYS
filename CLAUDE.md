<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# CLAUDE.md

**FerroSYS** is the shared system plumbing of the FerroHEALTH servers,
FerroBRIDGE, FerroEHR, FerroFED, FerroPIX and FerroTERM, as Rust library
crates: the logging console and the request log, OpenTelemetry export, the
health family with its indicator registry and the container healthcheck,
the HTTP server stack with the bounded drain, and configuration loading
with secret handling. It is the same kind of repository as FerroAUTH and
FerroAUDIT: shared crates that run inside the products and take no call
(`.claude/memory/family-architecture.md`). FerroSYS distributes no
configuration, deploys nothing and runs no service, by the
owner's ruling of 2026-10-26 (`.claude/memory/ferrosys.md`).

FerroSYS in prose, `ferrosys` in identifiers that are not crate names. The
crate name `ferrosys` on crates.io belongs to an unrelated project, so no
package here takes it.

## State of the repository

No code yet. The repository holds the licence, the brand, the tracker
tooling, the contribution-licence guard and the plan in
`docs/architecture.md`: six `ferrohealth-*` crates, their policies and the
migration order across the five products. Read it first. Nothing is
scaffolded before its issue is filed and its milestone set, and no crate is
built before its crates.io name is reserved, which needs the owner.
Describe only what exists; planned work lives on the milestones, never as a
prose promise in the README.

## What belongs here, and what does not

- **Here:** process plumbing every server would otherwise write for itself:
  the console format and filter, the request log that names the matched
  route and never a body, OTLP traces and metrics, liveness and readiness,
  the request id, the panic catch, timeouts and body limits, the console
  security headers, the configuration file and environment overrides with
  `_file` secrets, the shutdown drain, the usage report to FerroPULSE.
- **Not here:** a product's domain (FHIR, OMOP, openEHR, federation,
  identity, terminology), authentication, the policy decision and the
  console sign-in and session (FerroAUTH), the audit trail and the EHDS access record (FerroAUDIT), and
  UI components (FerroUI). When FerroSYS needs one of those, it depends on
  the owning crate; no FerroSYS crate is ever a copy of one.
- **No dependency cycle:** a crate here that FerroAUTH or FerroAUDIT
  depends on never depends back on them.

## IMPORTANT hard rules

- **The standards are the authority** once code lands: the OpenTelemetry
  protocol and semantic conventions, W3C Trace Context, the HTTP RFCs, and
  the FerroPULSE report envelope, vendored and pinned. Cite the section in
  every `// NOTE:`; never resolve a question from memory or from another
  implementation's behaviour. Where none governs a decision, say so: "no
  specification governs this: our own design".
- **No secret, token, password or clinical payload reaches a log line, a
  `Debug` rendering or an error message.** Every secret type redacts itself,
  and a URL is rendered with its userinfo masked.
- **Never send the user's email address to any service**, a `User-Agent`
  header included.
- **Prose follows the family writing style:** no em dashes, no "not X but
  Y", no decorative triads, no filler words. Comments follow RFC 505 and RFC
  1574 with hard budgets: `// TODO(#NNNN):` names its issue, `// NOTE:` is a
  citation and one sentence.
- **Branches use conventional types** (`feat/`, `fix/`, `chore/`, `docs/`,
  `refactor/`, `perf/`, `test/`, `ci/`, `build/`, `release/`); every commit is
  signed; `main` takes changes only through pull requests.
- **Every pull request ticks the contribution-licence box**
  (`.claude/memory/pr-licence-box.md`); the `contribution-licence-guard`
  check reads it.
- **NEVER add AI or Claude attribution** to a commit, PR, issue, comment, or
  code comment.
- **Never weaken, skip, or delete a test** to make a build pass.

## Repo map

- `Cargo.toml`: the workspace, with the family lint set and the shared
  dependency table; each crate keeps its own `version`.
- `crates/<crate>/`: one directory per crate, each with its own
  `CHANGELOG.md` and released by the tag `<crate>-vX.Y.Z`.
- `docs/architecture.md`: the design of record, the plan for the six crates.
- `assets/brand/`: the gauge mark, the steel lockup and the "Olive & Iron" tokens.
- `scripts/gh/`: the tracker helpers (`fields.sh` for the issue type and the
  `Priority` and `Effort` fields, `labels.sh`, `rel.sh` for sub-issue and
  dependency edges).
- `scripts/checks/`: the committed guards (comment style, licence headers,
  `TODO(#N)` issues, the changelog structure and Security rules, the crate
  version rule, the advisory exceptions, the contribution-licence box).
- `scripts/release/`: the release plan, the preflight, the milestone check
  and close, and the crates.io publish and read-back (`docs/release.md`).
- `.github/`: CI with the single required `conclusion` job (it also runs on
  `merge_group`), the PR guard re-run, CodeQL, Scorecard, Dependabot, the
  release lane (`release.yml` with the reusable `release-crate.yml`, and the
  manual `publish-crates.yml`, both under the `crates-io` environment), the
  PR template and the bug, feature and task forms.
- `.claude/rules/issue-workflow.md`: the tracker loop, the issue title rule and
  the label model.
- `.claude/memory/`: the working memory; `MEMORY.md` is the index.

## Issue workflow

The tracker is GitHub Issues. Type (Bug, Feature, Task), priority and effort
are GitHub's native issue type and the organisation's `Priority` and
`Effort` fields, set with `scripts/gh/fields.sh`; file every issue with
`scripts/gh/fields.sh new`. An issue title carries no conventional-commit
prefix (a PR title does), and every issue carries its crate label, a
specification label where one applies and, for a Task, one work-kind label
(`.claude/rules/issue-workflow.md`). Read an issue with
`gh issue view <n> --json title,body,comments`, never `--comments`. A PR
declares `Closes #N`, one keyword per issue. Milestones are releases.

## Licence

Business Source License 1.1 (`LICENSE`, `NOTICE`), Licensor Cadasto B.V.,
Change License Apache 2.0 four years after each version; every first-party
file carries `SPDX-FileCopyrightText: Cadasto B.V.` and
`SPDX-License-Identifier: BUSL-1.1` (`.claude/memory/license-busl.md`).
Vendored material keeps its upstream terms, recorded beside it.
