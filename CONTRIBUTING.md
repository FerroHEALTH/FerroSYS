<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Contributing to FerroSYS

FerroSYS holds the system plumbing the FerroHEALTH servers share: logging,
OpenTelemetry, health checks, the HTTP server stack and configuration, as
Rust libraries. It has no code yet. The design happens on the tracker of this
repository, and the tracker is the record of it. The working discipline is
[`CLAUDE.md`](CLAUDE.md); read it before making a change. The conventions
the family shares apply from the first commit, and the contribution terms
every FerroHEALTH product shares are in the family book at
<https://ferrohealth.eu/docs/licensing.html#contributions>.

## Issues

The type of an issue (Bug, Feature, Task) is GitHub's native issue type, and
its priority (Urgent, High, Medium, Low) and effort (High, Medium, Low) are
the FerroHEALTH organisation's `Priority` and `Effort` issue fields; none of
the three is a label. The bug, feature and task forms set the type, and a
maintainer sets the rest at triage with `scripts/gh/fields.sh`, which also
files a new issue with all three (`scripts/gh/fields.sh new <type>
<priority> <effort> <gh issue create args>`). A Task carries exactly one
work-kind label (`documentation`, `chore`, `refactor`, `perf`, `test`, `ci`),
which names its branch and commit type; a Bug is a `fix` and a Feature a
`feat`. `scripts/gh/labels.sh` bootstraps the labels, and `scripts/gh/rel.sh`
sets sub-issue and dependency edges. Read an issue with
`gh issue view <n> --json title,body,comments`; `--comments` prints nothing
for an issue without comments.

## Build and test

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --all-features --locked
cargo test --doc --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps --document-private-items
cargo deny check
```

A change to a workflow or a shell script also needs the linters CI runs:

```
zizmor --min-severity=low .github/
actionlint
shellcheck --severity=style <the shell files you touched>
```

## Pull requests

- Branch from `main` with a conventional-type name (`feat/`, `fix/`, `chore/`,
  `docs/`, `refactor/`, `perf/`, `test/`, `ci/`, `build/`, `release/`).
- Every commit is signed, and the pull request body declares `Closes #<n>` for
  the tracker issue it answers, one `Closes` keyword per issue.
- No AI or assistant attribution anywhere in the commits or the pull request.
- Every first-party file carries the SPDX header
  (`SPDX-FileCopyrightText: Cadasto B.V.`,
  `SPDX-License-Identifier: BUSL-1.1`).

## Licensing of contributions

FerroSYS's own code is licensed under the Business Source License 1.1
([`LICENSE`](LICENSE)). By submitting a contribution you:

1. certify that you wrote it, or otherwise have the right to submit it under
   these terms;
2. license it under the Business Source License 1.1 as applied to the version it
   lands in, including that version's Change License, so it becomes Apache 2.0
   with the rest of that version; and
3. grant the Licensor named in `LICENSE` a perpetual, irrevocable, worldwide,
   royalty-free, transferable right to use, reproduce, modify, distribute,
   sublicense and relicense the contribution as part of the Licensed Work under
   any terms, including commercial licences.

You keep your copyright. Point 3 is what lets the Licensed Work stay one work
with one licensor: a commercial licence, a change of the licence parameters, or a
transfer of the project can then cover every line, not only the maintainer's own.
There is no separate agreement to sign: the pull request template carries a
checkbox recording your acceptance of these terms, and a pull request from a
person does not merge without it (the `contribution-licence-guard` check, backed
by `scripts/checks/contribution-licence.sh`).

## Security

Report a vulnerability privately through the address in
[`SECURITY.md`](SECURITY.md), never in a public issue.
