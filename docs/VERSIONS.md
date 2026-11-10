<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Pinned inputs

The external inputs FerroSYS vendors, each fetched by its
`scripts/vendor/*.sh` from the pin below and stamped with a `PROVENANCE.md`
beside the copy. Move a pin here, re-run the script, and commit both.

| Input | Pin | Copy | Script |
|---|---|---|---|
| FerroPULSE report envelope v2 | `FerroHEALTH/FerroPULSE` commit `2060978b7ab139c3cd163c8ddb0c280bcffda31a`, schema sha256 `e609f17a5bfa78c6c08bdd3a28025889e20e0fde5c72fa881e7ed47b6abc8bc7` | `docs/specs/ferropulse/` | `scripts/vendor/ferropulse-schemas.sh` |
| OpenTelemetry semantic conventions | `open-telemetry/semantic-conventions` release `v1.44.0`, commit `e10a930844c6951757a43b849d364f7d056ac32b` | `docs/specs/otel-semconv/` | `scripts/vendor/otel-semconv.sh` |
| IETF RFC 5280 | `https://www.rfc-editor.org/rfc/rfc5280.txt` sha256 `a2f2628c0a83b873fc4786abd921f9b2c02395954b655d190bf16b831633345d`, the X.509 certificate and CRL profile | `docs/specs/ietf-pkix/` | `scripts/vendor/ietf-pkix.sh` |
| IETF RFC 6960 | `https://www.rfc-editor.org/rfc/rfc6960.txt` sha256 `7e63ffa1ea2ce2737d9aaf895a63776e9105ddbdfc97b8ae4029e3e825b4cdea`, the Online Certificate Status Protocol | `docs/specs/ietf-pkix/` | `scripts/vendor/ietf-pkix.sh` |

## Test images

The images a CI job starts beside the tests, pinned by tag and by the index
digest the registry answered on the day of the pin.

| Image | Pin | Used by |
|---|---|---|
| PostgreSQL | `postgres:18.6@sha256:bd5dfa494a5a259704365813dbfa5fa6b3e69be7348d43b547f540c730478583` (2026-11-06) | the `services` of the `nextest` job in `.github/workflows/ci.yml` and the `analyze` job in `.github/workflows/sonar.yml`, for the `pg_stat_database` cases of `ferrohealth-perf` |

## Tools

The tools a check runs at a pinned version. CI installs each through
`taiki-e/install-action`, which verifies the release archive against the
checksum in its manifest; the check refuses any other installed version.

| Tool | Pin | Check |
|---|---|---|
| cargo-semver-checks | `0.51.0` | `scripts/checks/semver.sh` (CI on every pull request and in the merge queue, `scripts/release/plan.sh` at every release) |

The clone detector of `scripts/checks/duplication.sh` also runs on a
developer machine, so its pins live here and the guard reads them from this
table: the release, the official image of that release by digest, and the
sha256 of each release archive `scripts/checks/duplication.sh --install`
fetches into `target/tools/` for a machine with neither the binary on `PATH`
nor docker or podman. CI runs the image. The guard refuses a binary that
answers another version. Bumping jscpd moves every row together, plus a
fresh `--update` of the count when the new release counts differently.

| Tool | Pin | Check |
|---|---|---|
| `jscpd` | `5.4.2` (release v5.4.2 of github.com/kucherenko/jscpd, 2026-10-20) | `scripts/checks/duplication.sh` |
| `jscpd image` | `ghcr.io/kucherenko/jscpd:5.4.2@sha256:112bc771493f25ec412ebf61bef738dc1fbe80d3793090e6a5eeac827ef0ee79` | `scripts/checks/duplication.sh` (docker, podman) |
| `jscpd darwin-arm64` | `b51d22d7c7610d27d8fd022577021ff3266a10f13295a2e545a8b8e126133a88` (`jscpd-darwin-arm64.tar.gz`) | `scripts/checks/duplication.sh --install` |
| `jscpd darwin-x64` | `ae8d24aba2719df7682b1bb2c5021ab096116b8240c45fdae88654fc1ff36b17` (`jscpd-darwin-x64.tar.gz`) | `scripts/checks/duplication.sh --install` |
| `jscpd linux-x64` | `5f18a6b70b4210782fc3c401c780ba994a2777249cd44d97d6fa460f6fff4570` (`jscpd-linux-x64-musl.tar.gz`) | `scripts/checks/duplication.sh --install` |
| `jscpd linux-arm64` | `3635d652ae08ae10bfbc6821bfa8ac14080101f317fb5f45f4b590d5d32b9339` (`jscpd-linux-arm64-musl.tar.gz`) | `scripts/checks/duplication.sh --install` |
