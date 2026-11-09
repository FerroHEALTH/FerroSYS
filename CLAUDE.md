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

The six library crates of `docs/architecture.md` exist under `crates/` and
are on crates.io, each released on its own version line by its own tag
(`docs/release.md`): `ferrohealth-secret`, `ferrohealth-config`,
`ferrohealth-telemetry`, `ferrohealth-health`, `ferrohealth-server` and
`ferrohealth-usage-report`. Beside them, `ferrohealth-perf` is the
performance gate a product's CI job runs against its server (#249), a CLI
on its own version line that no product links. Each crate's `Cargo.toml` and `CHANGELOG.md`
name its version and what it shipped. Planned work lives on the milestones,
one per crate release, never as a prose promise in the README; describe
only what exists. A new crate is added only after its issue is filed, its
milestone set and its crates.io name reserved, which needs the owner.

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

- **Family rules carried in `.claude/memory/` (index `MEMORY.md`).** A removal
  aid (a named refusal of a removed or renamed config key, flag, env variable,
  route or format) lives exactly one release and the next release deletes it
  fully; no legacy support is ever kept (owner hard rule 2026-11-09,
  `no-compat-shims.md`). Build the proper solution even when it breaks an API
  and moves the minor version (owner 2026-11-08, `no-half-solutions.md`).
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
- **NEVER add AI or Claude attribution** to a commit, PR, issue, comment, or
  code comment.
- **Never weaken, skip, or delete a test** to make a build pass.

## Repo map

- `Cargo.toml`: the workspace, with the family lint set and the shared
  dependency table; each crate keeps its own `version`.
- `crates/<crate>/`: one directory per crate, each with its own
  `CHANGELOG.md` and released by the tag `<crate>-vX.Y.Z`.
- `tools/forwarders`: the `syn` detector of the forwarder ratchet, a
  workspace member that publishes nothing and depends on no workspace crate.
- `docs/architecture.md`: the design of record, the plan for the six crates
  and the performance gate.
- `assets/brand/`: the gauge mark, the steel lockup and the "Olive & Iron" tokens.
- `scripts/gh/`: the tracker helpers (`fields.sh` for the issue type and the
  `Priority` and `Effort` fields, `labels.sh`, `rel.sh` for sub-issue and
  dependency edges).
- `scripts/checks/`: the committed guards (comment style, licence headers,
  `TODO(#N)` issues, the changelog structure and Security rules, the crate
  version rule, the advisory exceptions, and
  `semver.sh`, cargo-semver-checks against crates.io), and the three ratchets
  of `.claude/rules/rust-style.md` that fail a rise and pass a fall:
  `duplication.sh`, jscpd at 50 tokens over 5 lines holding the duplicated
  lines to `duplication.count` with its baseline of block keys, and
  `file-size.sh`, the 800-line budget with its allow-list
  `file-size-allow.txt`, and `forwarders.sh`, which holds the functions that
  only forward to another to `forwarders.count` with its baseline of keys.
- `scripts/release/`: the release plan, the preflight, the milestone check
  and close, and the crates.io publish and read-back (`docs/release.md`).
- `.github/`: CI with the single required `conclusion` job (it also runs on
  `merge_group`), the PR guard re-run, CodeQL, SonarQube Cloud (`sonar.yml`,
  advisory, its scope in `sonar-project.properties`), Scorecard, Dependabot, the
  release lane (`release.yml` with the reusable `release-crate.yml`, and the
  manual `publish-crates.yml`, both under the `crates-io` environment),
  `ratchet-tighten.yml`, which lowers the ratchet baselines on `main` once a
  day through one branch a maintainer opens as a pull request, the PR
  template and the bug, feature and task forms.
- `.claude/rules/issue-workflow.md`: the tracker loop, the issue title rule and
  the label model.
- `.claude/rules/rust-style.md`: code is written once, the 800-line file
  budget and no function only forwards to another, each with its ratchet.
- `.claude/hooks/rust_edit.sh`: the per-edit hook `.claude/settings.json`
  runs on every edited `.rs` file (the file-size and forwarder ratchets).
- `.claude/rules/crates-publishing.md`: the caret requirements between the
  crates and the semver check that keeps every patch compatible.
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
