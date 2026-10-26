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
