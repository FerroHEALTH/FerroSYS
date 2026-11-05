<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the IETF PKIX RFCs

- Source: the RFC Editor, <https://www.rfc-editor.org/>, the plain-text
  publication of each RFC
- Pins, one row of `docs/VERSIONS.md` per RFC, by URL and sha256:
- RFC 5280: <https://www.rfc-editor.org/rfc/rfc5280.txt>, sha256 `a2f2628c0a83b873fc4786abd921f9b2c02395954b655d190bf16b831633345d`
- RFC 6960: <https://www.rfc-editor.org/rfc/rfc6960.txt>, sha256 `7e63ffa1ea2ce2737d9aaf895a63776e9105ddbdfc97b8ae4029e3e825b4cdea`
- Fetched: 2026-11-05 by `scripts/vendor/ietf-pkix.sh`
- Licence: each RFC is subject to BCP 78. The IETF Trust Legal Provisions
  (<https://trustee.ietf.org/license-info>) grant every person the right to
  copy, publish, display and distribute IETF Documents in full and without
  modification; the files are those documents, in full and unmodified, and
  keep their upstream terms
- Read by: the revocation check of `ferrohealth-server` (feature
  `revocation`), which cites RFC 5280 §4.2.1.13, §5 and §6.3 for its CRL
  check and RFC 6960 for its OCSP check

Never hand-edit these files; change the pins in `docs/VERSIONS.md` and
re-run the script.

| File | sha256 |
|---|---|
| `rfc5280.txt` | `a2f2628c0a83b873fc4786abd921f9b2c02395954b655d190bf16b831633345d` |
| `rfc6960.txt` | `7e63ffa1ea2ce2737d9aaf895a63776e9105ddbdfc97b8ae4029e3e825b4cdea` |
