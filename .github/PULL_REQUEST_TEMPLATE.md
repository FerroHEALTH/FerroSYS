<!-- SPDX-FileCopyrightText: Vernum Projecten B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

## What changed and why

<!-- Describe the change and the reason for it. Keep it to what a reviewer needs. -->

Closes #NNN

## Licensing of contributions

- [ ] I accept the terms in [CONTRIBUTING.md § Licensing of contributions](../CONTRIBUTING.md#licensing-of-contributions): I have the right to submit this work, I license it under the project licence of the version it lands in, and I grant the Licensor the relicensing right stated there.

## Checklist

- [ ] Shell and workflow files are clean: `shellcheck --severity=style`, `actionlint`, `zizmor --min-severity=low .github/`.
- [ ] Rust gates pass, once a workspace exists: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo deny check`.
- [ ] Docs are updated, if behaviour changed.
- [ ] Every commit is signed.
- [ ] No AI or assistant attribution anywhere in the commits or this PR.

Contributions carry the licensing terms in
[CONTRIBUTING.md](../CONTRIBUTING.md#licensing-of-contributions); the
`contribution-licence-guard` check reads the box above, and there is no separate
agreement to sign. See [CONTRIBUTING.md](../CONTRIBUTING.md) for the full
contribution guide.
