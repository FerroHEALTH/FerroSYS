<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Published crate discipline (crates.io)

Every crate under `crates/` is published on crates.io on its own version
line, released by its own tag `<crate>-vX.Y.Z` (`docs/release.md`).
Published versions are immutable, so the requirements between the crates
and the compatibility of each release are hard rules.

## Carets between the FerroSYS crates

- **One FerroSYS crate requires another by a caret requirement** on the
  newest release it needs, in `[workspace.dependencies]`:
  `ferrohealth-secret = { path = "crates/ferrohealth-secret", version =
  "0.2.0" }`, never `"=0.2.0"`. Cargo reads `"0.2.0"` as any `0.2.x` at or
  above it
  (<https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#caret-requirements>),
  so a product's lockfile takes a patch of one crate without waiting for a
  release of the crates built on it.
- The same rule holds for a requirement on another family shared crate
  (FerroAUTH, FerroAUDIT): a caret on the newest release this crate needs.
- **The products pin each shared crate exactly** (`ferrohealth-server =
  "=0.3.2"`) and move one pull request at a time; their lockfile is what
  makes a build reproducible. Each crate README's install line shows that
  exact pin, and the release plan checks it.
- A release moves a requirement in `[workspace.dependencies]` only when a
  dependant needs something the new release added.

## Every release is checked for compatibility

A caret is only safe when a patch never breaks the API. Cargo's SemVer
rules (<https://doc.rust-lang.org/cargo/reference/semver.html>) count a
`0.x.y` to `0.x.(y+1)` release as compatible, so a public API break on a
`0.x` line moves the minor version (`0.(x+1).0`).

`cargo-semver-checks`
(<https://github.com/obi1kenobi/cargo-semver-checks>) holds that by machine:
it builds the rustdoc of each crate and of its newest version on crates.io
and refuses a break the manifest version does not allow. An unchanged
version is checked as a change that breaks nothing. `scripts/checks/semver.sh`
runs it at the version pinned in `docs/VERSIONS.md`, twice per crate: on the
tool's default feature heuristic, and with `--only-explicit-features`, the
API a product built with `default-features = false` compiles against (the
tool's README lists a break that exists only when a subset of the features
is active among what one pass does not catch). Either pass failing fails
the crate, and a judge that could not build is named apart from a break.
It runs:

- on every pull request and in the merge queue whenever the change reaches
  a crate, over every crate past 0.0.0 (the `cargo-semver-checks` job of
  `ci.yml`, `semver.sh --changed` against the first parent), so a breaking
  change carries its minor bump in the pull request that breaks. A change
  that reaches no crate leaves every API as its base has it, and is not
  judged: judging it would judge only the base, against whatever crates.io
  holds when the run starts. A crate with no release on crates.io has
  nothing to compare with, and the job's output names it;
- in the release plan (`scripts/release/plan.sh`, which
  `scripts/release/preflight.sh` and the `plan` job of `release.yml` both
  run), over the crate the tag releases.

A failure is answered with the minor bump and a changelog entry naming what
a product changes, or by keeping the API; never by skipping the check.

## A public dependency on another line is a break

A crate that exposes the types of a public dependency, a `ferrohealth-*`
crate, breaks when it moves that requirement to another compatibility line
(`0.4` to `0.5`): the exposed types become other types, and code that built
against the crate stops building, which The Cargo Book's "SemVer
Compatibility" introduction counts as a breaking change. The
`cargo-semver-checks` README lists "breaking type changes, for example in
the type of a field or function parameter" among what the tool does not
catch, so `scripts/lib/public-deps.sh` compares each such requirement's
line itself, after FerroBRIDGE's `scripts/checks/lib/public-deps.sh`.
`scripts/checks/crate-version-guard.sh` compares the pull request's base
with its head, and `semver.sh` the crate's newest release with the tree;
both ask for the next minor at patch 0 (SemVer 2.0.0 §7) when a line
moved. Which
dependencies count as public is our own design: every `ferrohealth-*`
requirement, since no specification says which of its items a crate
exposes.

## Official documentation (durable citations)

- Caret requirements: <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#caret-requirements>
- SemVer compatibility: <https://doc.rust-lang.org/cargo/reference/semver.html>
- What cargo-semver-checks does not catch: <https://github.com/obi1kenobi/cargo-semver-checks#will-cargo-semver-checks-catch-every-semver-violation>
- cargo-semver-checks: <https://github.com/obi1kenobi/cargo-semver-checks>
- SemVer 2.0.0 §7: <https://semver.org/#spec-item-7>
- `cargo publish`: <https://doc.rust-lang.org/cargo/commands/cargo-publish.html>
