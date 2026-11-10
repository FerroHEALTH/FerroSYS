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
  makes a build reproducible. Each crate README's install line and the
  crate's line in the root README show that exact pin, and the release plan
  checks both.
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

A crate that exposes the types of a dependency breaks when it moves that
requirement to another compatibility line (`0.4` to `0.5`, `1` to `2`):
the exposed types become other types, and code that built against the
crate stops building. The introduction of The Cargo Book's "SemVer
Compatibility" chapter names these as the breaks it is about: "changes that
will cause cargo and rustc to fail to build something that previously
worked". The `cargo-semver-checks` README lists
"breaking type changes, for example in the type of a field or function
parameter" among what the tool does not catch, so `scripts/lib/public-deps.sh`
compares each such requirement's line itself, after FerroBRIDGE's
`scripts/checks/lib/public-deps.sh`.
`scripts/checks/crate-version-guard.sh` compares the pull request's base
with its head, and `semver.sh` the crate's newest release with the tree;
both ask for the next minor at patch 0 (SemVer 2.0.0 §7) when a line
moved. Neither asks more than that: a minor that skips a line, or a minor
with no break, passes.

A public dependency is any of these:

- every `ferrohealth-*` requirement and every sibling crate of this
  workspace, optional ones included, since the rule assumes a crate exposes
  them all;
- every third-party crate the crate declares in the `public-dependencies`
  array of its `[package.metadata.ferrohealth]` table: `rustls` and `axum`
  for `ferrohealth-server`, whose `RevocationCheck::config()` hands out a
  `rustls::ClientConfig` and whose router is an `axum::Router`.

`scripts/checks/public-surface.sh` keeps the declaration true. In the
`lint` job it builds each crate's rustdoc JSON with every feature, collects
the crates whose items the public API names (a signature, a field, a bound,
a trait implementation, a re-export), attributes a crate that is not a
direct dependency (`axum_core`, `serde_core`, `rustls_pki_types`) to each
direct dependency whose source re-exports it, and fails when the
declaration misses an attributed crate or names one the API does not
reach. It leaves out the standard library, the blanket and auto-trait
implementations rustdoc adds, and paths through a `__`-prefixed module,
the hidden support of a macro. A dependency either side of a pull request
declares counts for the guard, so dropping the declaration in the change
that moves the line does not hide the move. Which items make a dependency
public is our own design: no specification says which of its items a crate
exposes.

## Official documentation (durable citations)

- Caret requirements: <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#caret-requirements>
- SemVer compatibility: <https://doc.rust-lang.org/cargo/reference/semver.html>
- What cargo-semver-checks does not catch: <https://github.com/obi1kenobi/cargo-semver-checks#will-cargo-semver-checks-catch-every-semver-violation>
- The rustdoc JSON output: <https://doc.rust-lang.org/rustdoc/unstable-features.html#json-output>
- cargo-semver-checks: <https://github.com/obi1kenobi/cargo-semver-checks>
- SemVer 2.0.0 §7: <https://semver.org/#spec-item-7>
- `cargo publish`: <https://doc.rust-lang.org/cargo/commands/cargo-publish.html>
