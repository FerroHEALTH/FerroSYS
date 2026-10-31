<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

## What changed and why

<!-- Describe the change and the reason for it. Keep it to what a reviewer needs. -->

Closes #NNN

## Checklist

- [ ] Shell and workflow files are clean: `shellcheck --severity=style`, `actionlint`, `zizmor --min-severity=low .github/`.
- [ ] Rust gates pass, once a workspace exists: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo nextest run --workspace --locked`, `cargo deny check`.
- [ ] Docs are updated, if behaviour changed.
- [ ] Every commit is signed.
- [ ] No AI or assistant attribution anywhere in the commits or this PR.
