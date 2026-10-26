<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the FerroPULSE report envelope v2

- Source: <https://github.com/FerroHEALTH/FerroPULSE> (private; Cadasto B.V., the Licensor
  of this repository too)
- Commit: 2060978b7ab139c3cd163c8ddb0c280bcffda31a
- Schema sha256: e609f17a5bfa78c6c08bdd3a28025889e20e0fde5c72fa881e7ed47b6abc8bc7
- Fetched: 2026-10-26 by `scripts/vendor/ferropulse-schemas.sh`
- Licence: upstream declares `LicenseRef-Proprietary` in its `Cargo.toml`
  and ships no LICENSE file; Cadasto B.V. holds the copyright of both
  repositories and publishes this copy under this repository's licence
- Contents: `schemas/report-envelope.v2.json` (JSON Schema 2020-12) and
  `schemas/examples/report-envelope.v2/` (the contract's own valid and
  invalid examples), 68 files, copied verbatim

The ferrohealth-usage-report tests check the validator against the
examples, then validate every report the crate builds against the schema.
Never hand-edit these files; change the pin in `docs/VERSIONS.md` and
re-run the script.
