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
version is checked as a patch. `scripts/checks/semver.sh` runs it at the
version pinned in `docs/VERSIONS.md`:

- on every pull request, over the workspace (the `cargo-semver-checks` job
  of `ci.yml`), so a breaking change carries its minor bump in the pull
  request that breaks;
- in the release plan (`scripts/release/plan.sh`, which
  `scripts/release/preflight.sh` and the `plan` job of `release.yml` both
  run), over the crate the tag releases.

A failure is answered with the minor bump and a changelog entry naming what
a product changes, or by keeping the API; never by skipping the check.

## Official documentation (durable citations)

- Caret requirements: <https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#caret-requirements>
- SemVer compatibility: <https://doc.rust-lang.org/cargo/reference/semver.html>
- cargo-semver-checks: <https://github.com/obi1kenobi/cargo-semver-checks>
- `cargo publish`: <https://doc.rust-lang.org/cargo/commands/cargo-publish.html>
