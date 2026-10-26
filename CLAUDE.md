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
(`.claude/memory/family-architecture.md`). A central log and console
service, the central half of the logging component, is later work in this
repository, built on the crates once the products use them.

FerroSYS in prose, `ferrosys` in identifiers that are not crate names. The
crate name `ferrosys` on crates.io belongs to an unrelated project, so no
package here takes it.

## State of the repository

No code yet. The repository holds the licence, the brand, the tracker
tooling and the contribution-licence guard. The crate layout, the public
API and the migration order across the five products are open for the
owner's decision; nothing is scaffolded before its issue is filed and its
milestone set. Describe only what exists; planned work lives on the
milestones, never as a prose promise.

## What belongs here, and what does not

- **Here:** process plumbing every server would otherwise write for itself:
  the console format and filter, the request log that names the matched
  route and never a body, OTLP traces and metrics, liveness and readiness,
  the request id, the panic catch, timeouts and body limits, the console
  security headers, the configuration file and environment overrides with
  `_file` secrets, the shutdown drain.
- **Not here:** a product's domain (FHIR, OMOP, openEHR, federation,
  identity, terminology), authentication and the policy decision
  (FerroAUTH), the audit trail and the EHDS access record (FerroAUDIT), and
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

- `assets/brand/`: the gauge mark, the lockup and the "Olive & Iron" tokens.
- `scripts/gh/`: the tracker helpers (`fields.sh` for the issue type and the
  `Priority` and `Effort` fields, `labels.sh`, `rel.sh` for sub-issue and
  dependency edges).
- `scripts/checks/contribution-licence.sh` and
  `.github/workflows/contribution-licence.yml`: the licence-box guard.
- `.github/`: the PR template and the bug, feature and task forms.
- `.claude/memory/`: the working memory; `MEMORY.md` is the index.

## Issue workflow

The tracker is GitHub Issues. Type (Bug, Feature, Task), priority and effort
are GitHub's native issue type and the organisation's `Priority` and
`Effort` fields, set with `scripts/gh/fields.sh`; file every issue with
`scripts/gh/fields.sh new`. Read an issue with
`gh issue view <n> --json title,body,comments`, never `--comments`. A PR
declares `Closes #N`, one keyword per issue. Milestones are releases.

## Licence

Business Source License 1.1 (`LICENSE`, `NOTICE`), Licensor Cadasto B.V.,
Change License Apache 2.0 four years after each version; every first-party
file carries `SPDX-FileCopyrightText: Cadasto B.V.` and
`SPDX-License-Identifier: BUSL-1.1` (`.claude/memory/license-busl.md`).
Vendored material keeps its upstream terms, recorded beside it.
