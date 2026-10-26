<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Cutting a release

No specification governs the release procedure: our own design. Every crate
under `crates/` releases on its own version line:

| Crate | Tag | Version | Changelog |
|---|---|---|---|
| `<crate>` | `<crate>-vX.Y.Z` | `[package]` of `crates/<crate>/Cargo.toml` | `crates/<crate>/CHANGELOG.md` |

The crates are `ferrohealth-secret`, `ferrohealth-config`,
`ferrohealth-telemetry`, `ferrohealth-health`, `ferrohealth-server` and
`ferrohealth-usage-report` (`docs/architecture.md`). A crate holds its name
at 0.0.0 until its first release.

A release is the signed tag on the merge commit of the release pull request
on `main`; `.github/workflows/release.yml` does the rest for the crate the
tag names. Below, `<tag>` is that tag and the steps name the crate's own
version and changelog. Every release rolls out to every FerroHEALTH product
at once, one pull request per product (lockstep).

## The release pull request

On the branch `release/<tag>`:

1. **The milestone is empty.** Every issue and pull request of the milestone
   titled as the tag is closed or moved; the release refuses the tag
   otherwise (`scripts/release/milestone.sh check <tag>`).
2. **Bump the version.** Set the crate's version and run `cargo update -w`.
   A release that another workspace crate must follow also moves that
   crate's requirement in `[workspace.dependencies]`.
3. **Cut the changelog.** Rename `## [Unreleased]` to
   `## [X.Y.Z] - YYYY-MM-DD` and open a fresh, empty `## [Unreleased]` above
   it. The date is the day of the cut: the release notes count the support
   period from it. Refresh the link references at the foot: `[Unreleased]`
   compares from the new tag, and `[X.Y.Z]` compares the previous tag with
   it (a first release links `releases/tag/<tag>`). Read the section as the release notes, and reread the reason of a
   Security section (§ Security entries).
4. **The gates pass.** The crate-version guard checks that the version, the
   lock and the changelog section agree.

## Security entries

Two rules of Regulation (EU) 2024/2847 hold every Security section, and
`scripts/checks/changelog-security.sh` checks both: CI runs it on every pull
request over `[Unreleased]`, and the release `plan` job over the section it
cuts (`scripts/release/plan.sh`). A section released before the rules is
never checked again.

- **Every entry names its advisory** (Annex I Part II(4), (8)). A Security
  entry carries the `GHSA-` id of the FerroSYS security advisory drafted
  for the fix and published with the release (`SECURITY.md`, Security
  advisories), or, for a dependency finding an OpenVEX statement shows does
  not affect FerroSYS, the path of that statement under `security/vex/`,
  which must exist. A RUSTSEC id alone is not enough: name it beside the
  advisory.
- **A security fix ships alone, or says why not** (Annex I Part II(2)). A
  release whose Security section sits beside any other section opens that
  section with one paragraph, `Shipped with functional changes because:`
  and the reason on the same line, naming why a security-only patch of the
  newest release was not the way to ship it. Whoever adds the first
  Security entry beside other pending changes writes it, and whoever cuts
  the release rereads it.

`scripts/checks/changelog-structure.sh` holds every section to the six Keep a
Changelog types, each at most once.

## Preflight

The `plan` job verifies every release fact at the tagged commit before
anything is built or published. Its checks are one list,
`scripts/release/plan.sh`, and `scripts/release/preflight.sh` runs the same
list on your machine:

```sh
scripts/release/preflight.sh <tag>
scripts/release/preflight.sh <tag> --notes /tmp/notes.md   # keep the notes
```

Run it on `release/<tag>` once the cut is committed, and again on `main`
after the merge, at the commit you are about to tag. It exits 0 when the
`plan` job would pass, names every check that refuses, and prints the path
of the release notes it composed, so you read them before they are
published. You need `gh` logged in, because the milestone check reads the
tracker.

| Check | Script |
|---|---|
| The milestone titled as the tag holds no open item | `scripts/release/milestone.sh check` |
| The crate's version names the tag's version | `plan.sh` |
| Every section of the changelog holds Keep a Changelog types, each once | `scripts/checks/changelog-structure.sh` |
| `[Unreleased]` compares from the tag, and the cut version has its link | `plan.sh` |
| The section being cut opens its `### Security` with the reason when it sits beside other changes, and every Security entry names a GHSA id or an existing `security/vex/` path | `scripts/checks/changelog-security.sh --section X.Y.Z` |
| The changelog section becomes the notes, with the support period appended: the month five years after the heading's date (Regulation (EU) 2024/2847 Art 13(19)) | `plan.sh notes` |

`scripts/release/plan.sh prev-tag <tag>` prints the previous release by
version order from the tags of origin, so a tag pushed whose release never
published still counts. To add a release fact, add a function and a line to
`list()` in `scripts/release/plan.sh`; the `plan` job and the preflight
both pick it up.

## The tag

After the release pull request merges and the preflight passes on `main`,
tag its merge commit signed and push the tag:

```sh
git tag -s <tag> -m <tag> <merge-commit>
git push origin <tag>
```

`release.yml` runs the preflight's list at the tag, takes the notes from
the `[X.Y.Z]` section with the support period appended, attests and attaches
each crate, publishes the release, and its `crates` leg waits on the
`crates-io` environment's reviewer. Its last job closes the milestone.

## Repository settings the lane needs

The owner sets these once; the lane fails loud without them.

- **The `crates-io` environment**, with the owner as required reviewer and
  a deployment rule that admits only the release tags: a tag rule
  `ferrohealth-*-v*`. Both `release.yml` (its `crates` job) and
  `publish-crates.yml` (its `publish` job) run under it, the second
  dispatched at a release tag, so the tag rule admits both.
- **Trusted Publishing on crates.io**, per crate: repository
  `FerroHEALTH/FerroSYS`, environment `crates-io`, one publisher entry for
  workflow `release.yml` and one for `publish-crates.yml` (the recovery
  lane; crates.io matches the workflow file the OIDC token names), with
  "require trusted publishing" on.
- **A tag ruleset** over `ferrohealth-*-v*` that refuses deletion and update,
  so a pushed tag is never moved.
- **The `main` ruleset** requires the `conclusion` check of `ci.yml`, signed
  commits and pull requests; with the merge queue on, `ci.yml` also runs on
  `merge_group`. A
pushed tag is never moved or deleted; a bad cut ships forward as a new
patch version.

## The manual publish

`.github/workflows/publish-crates.yml` is a dry run unless it is dispatched
with `publish = true`. Publishing through it is the recovery path for a
release that already exists, when the `crates` leg of `release.yml` failed:
dispatch it at the release's tag, never at a branch. Its `guard`
job (`scripts/release/publish-crates.sh guard`) refuses, before the
`crates-io` approval is asked for, unless the run is at a tag, that tag is
the release tag of the crate it names followed by that crate's manifest version, and a published GitHub release exists
for it, and it names what is missing. A version reaches crates.io only
through a tag and its release, so the manual lane can never publish a
version the changelog and the tag do not carry.

## A security fix

A reported vulnerability gets a private draft GitHub security advisory the
day it is confirmed (`SECURITY.md`, Security advisories). The fix ships in a
patch release of the newest version, alone where that is technically
feasible. Its Security entry names the draft's `GHSA-` id. When the release
carries other changes too, the Security section opens with its reason
(§ Security entries). Publish the advisory, with the release version as its
patched version, in the same hour as the release.

`deny.toml` accepts no advisory today, and `scripts/checks/advisory-exceptions.sh`,
in the `cargo-deny` CI job, fails an exception that has outlived its
finding. The VEX document arrives with the first accepted advisory: that
change adds `security/vex/rust-advisories.toml`, the generated
`security/vex/rust-advisories.openvex.json`, `scripts/security/vex-generate.sh`,
`scripts/checks/vex-advisories.sh` and `scripts/checks/vex-reachability.sh`,
copied from FerroUI or FerroPIX, and a `vex` CI job. An empty document would
fail `vex-advisories.sh`, which requires a statement.
