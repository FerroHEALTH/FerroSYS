<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Rust style: one owner per concept

The rules that keep the hand-written Rust of the FerroSYS crates held to one
owner per concept, each with the guard that measures it. No specification
governs these: our own design, the rules FerroBRIDGE, FerroEHR and
VernumBOEK hold under the same names.

## Code is written once

- **Share knowledge and decisions, never text that only looks alike.** Two
  copies that change for the same reason (one business rule, one wire rule,
  one security rule) move to one owner in the same change. Two pieces with
  different reasons to change stay apart, however alike they read.
- **The owner is the module that owns the concern.** Across crates it is the
  crate lower in the layering (`ferrohealth-secret` below
  `ferrohealth-config` and `ferrohealth-telemetry`, `ferrohealth-telemetry`
  below `ferrohealth-server` and `ferrohealth-usage-report`). Within a crate
  it is the module whose part of the domain the code serves. It is never a
  `util`, `utils`, `helpers` or `common` module, whose name says nothing
  about what it holds, and never a function that takes a flag to choose
  between two behaviours.
- **One copy is fine, two are looked at, three are extracted.**
- **The ratchet measures it.** `scripts/checks/duplication.sh` runs jscpd
  (pinned by version and image digest in `docs/VERSIONS.md`, Tools) over
  `crates/*/src/` and `tools/*/src/` in the `duplication` CI job and holds
  the total of duplicated lines to the count in
  `scripts/checks/duplication.count`. A rise fails and names both places of
  each new block. A fall passes with a one-line note, and your pull request
  leaves the count alone: the `Ratchet tighten` workflow runs `--update` on
  `main` each day and pushes one branch with the lower count and
  `scripts/checks/duplication-baseline.txt`, the guard's own key of every
  counted block (its two paths and the sha256 of the lines at each, with no
  line number). `--update` never raises the count; a rise that has to stay
  is a hand edit of the file in a change that says why. Locally the guard
  takes a `jscpd` on `PATH` of the pinned version, then the pinned image
  through docker or podman, then the release binary
  `scripts/checks/duplication.sh --install` puts under `target/tools/`. No
  per-edit hook runs it: a move copies code before it deletes the original.
- **The threshold is 50 tokens over at least 5 lines**, jscpd's own
  defaults. The scan leaves out the `use` declarations, a file that opens
  with `// @generated`, a root `.gitattributes` marks generated, a file from
  its first column-one `#[cfg(test)]` line on (the inline test module), and
  `tests/` and `benches/`. Measured on 2026-11-04 at `bf8f674` with those
  exclusions, over 52 files and 22,649 lines:

  | Threshold | Blocks | Duplicated lines | Blocks across two files |
  | --- | --- | --- | --- |
  | 50 tokens, 5 lines | 5 | 43 | 2 |
  | 70 tokens, 5 lines | 3 | 25 | 2 |
  | 100 tokens, 5 lines | 0 | 0 | 0 |

  Every block sits in `ferrohealth-server`. Raising the threshold drops the
  short blocks (a repeated signature, a repeated generic bound list), yet
  each is code written twice as well, so 50 it is. Raising it is a change to
  this section and to the guard together, with the table measured again.

## File size

A hand-written Rust file is at most 800 lines. A file that opens with
`// @generated` is outside the rule, and so is a vendored tree under
`docs/specs/`. A file that grows toward the budget splits by concern into a
module folder: `<name>.rs` keeps the module doc, the `mod` declarations and
every public item at the path the crate root gives it, and one private child
file holds each concern below it. A split never moves a public item to a new
path, because that breaks the crate's API, and adds no `pub use` to keep an
old path. A test of private internals stays an inline `#[cfg(test)] mod
tests` in the child whose internals it reads, and a test of the public API
moves to the crate's `tests/`. A split moves code without changing
behaviour: the same tests pass before and after, and a defect found on the
way is filed, never fixed in the split.

`scripts/checks/file-size.sh` enforces it in the `file-size` CI job and per
edit through `.claude/hooks/rust_edit.sh`. A file over the budget fails
unless `scripts/checks/file-size-allow.txt` lists it, with its length and
the issue that splits it; a listed file passes only while it does not grow.
The list only tightens: a listed file that shrank, was split within the
budget or is gone passes with a one-line note, your pull request leaves the
list alone, and the `Ratchet tighten` workflow lowers or removes the entry
on `main` (`file-size.sh --update`, which never raises or adds an entry).

## No function only forwards to another

- **A function exists because it decides something.** It validates,
  branches, converts, owns a resource, or names a concept its callers
  need. A function whose body is a single call, with its arguments passed
  through or borrowed and at most `.await`, `?`, `Ok(..)`, `Some(..)`,
  `.into()` or an error-only `.map_err` around it, is a forwarder. It adds a
  name and a hop and decides nothing, so its callers call the target
  directly, and a crate-private API breaks to get there. A forwarder that
  fixes an argument (`self.read_of(section, TlsKeys::SERVER)`) is still one:
  its callers pass the argument.
- **Three kinds are not forwarders.**
  - The methods of a trait implementation (`Display`, `From`, `Deref` and
    the rest), which the trait requires. A binary's `main`, which the
    language requires, and a function a specification defines, whose doc
    comment cites its page (OpenTelemetry, W3C Trace Context or an RFC),
    stand with them.
  - A public function of a published `ferrohealth-*` crate, while removing
    it would break the crate's API: it stays, recorded in the baseline with
    that reason, until a decision takes the minor bump
    (`crates-publishing.md`).
  - A constructor (`new`, `with_*`, or an associated function that returns
    `Self`) that fixes an argument.
- **The ratchet measures it.** `tools/forwarders` parses every `.rs` file
  under `crates/*/src/` and `tools/*/src/` with `syn`, leaving out a file
  that opens with `// @generated`, anything under `#[cfg(test)]`, and
  `tests/`. It reports a free function or inherent method whose body is one
  call, to a function or to a method of `self`, whose arguments are
  parameters, literals and constants, or references, borrows and clones of
  those. A call that reads a field of `self`, or calls a method of a field
  or a parameter, reaches what the caller cannot, so an accessor is never
  reported; nor are the `#[cfg]` arms of one function.
  `scripts/checks/forwarders.sh` runs it in the `forwarders` CI job and per
  edit through `.claude/hooks/rust_edit.sh`, and holds the total to
  `scripts/checks/forwarders.count`. `scripts/checks/forwarders-baseline.txt`
  keys each counted forwarder by its path, owner and target call, with no
  line number, and a key may carry a tab and the reason the forwarder
  stays. A rise fails and names each new forwarder, its target and its
  place. A fall passes with a note, and the `Ratchet tighten` workflow
  lowers the count on `main`, as it does for the duplication ratchet.
  `--update` never raises the count and keeps each reason while its key
  stands.
