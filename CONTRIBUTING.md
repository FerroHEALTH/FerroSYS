<!-- SPDX-FileCopyrightText: Vernum Projecten B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Contributing to FerroSYS

FerroSYS is one of the [FerroHEALTH](https://ferrohealth.eu/) family and has no
code yet. The design happens on the tracker of this repository, and the
tracker is the record of it. The conventions the family shares apply from the
first commit.

## Pull requests

- Branch from `main` with a conventional-type name (`feat/`, `fix/`, `chore/`,
  `docs/`, `refactor/`, `perf/`, `test/`, `ci/`, `build/`, `release/`).
- Every commit is signed, and the pull request body declares `Closes #<n>` for
  the tracker issue it answers, one `Closes` keyword per issue.
- No AI or assistant attribution anywhere in the commits or the pull request.
- Every first-party file carries the SPDX header
  (`SPDX-FileCopyrightText: Vernum Projecten B.V.`,
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
