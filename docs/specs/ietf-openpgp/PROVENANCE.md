<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the IETF OpenPGP documents

- Source: the RFC Editor, <https://www.rfc-editor.org/>, and the IETF
  Internet-Draft archive, <https://www.ietf.org/archive/id/>, the plain-text
  publication of each document
- Pins, one row of `docs/VERSIONS.md` per document, by URL and sha256:
- IETF RFC 9580: <https://www.rfc-editor.org/rfc/rfc9580.txt>, sha256 `36540e57b63d24c0b17be0748b704bdeed3d643ce99a4fb1abb6469c1eb1ff18`
- IETF RFC 9980: <https://www.rfc-editor.org/rfc/rfc9980.txt>, sha256 `3d2b30bdaecaf28bfde85545bd24e661a97f1e7b371a963778d4314a678c93ac`
- IETF draft-dkg-openpgp-stateless-cli-16: <https://www.ietf.org/archive/id/draft-dkg-openpgp-stateless-cli-16.txt>, sha256 `3545c255c8af4db21757a6838ef06987bc0ec57889aa902f617e01ada5e48c53`
- Fetched: 2026-11-25 by `scripts/vendor/ietf-openpgp.sh`
- Licence: each document is subject to BCP 78. The IETF Trust Legal
  Provisions (<https://trustee.ietf.org/license-info>) grant every person
  the right to copy, publish, display and distribute IETF Documents in full
  and without modification; the files are those documents, in full and
  unmodified, and keep their upstream terms
- Read by: `ferrohealth-openpgp-pq`, which cites RFC 9580 and RFC 9980 for
  its keys and signatures, and whose `ferrohealth-sop` binary follows the
  Stateless OpenPGP draft at revision 16

The RFC 9980 Appendix A.3 vectors are extracted verbatim into
`crates/ferrohealth-openpgp-pq/tests/vectors/`, where the crate's tests read them:

| File | sha256 |
|---|---|
| `rfc9980-a3.1-secret-key.asc` | `515c09226777ed771ac0171576911d395d101dd1d9be094bbaefa4e5e1e03f19` |
| `rfc9980-a3.2-certificate.asc` | `4880dead81e043038df20e961ab74a7c61f14245371d6a229c1888be79813764` |
| `rfc9980-a3.4-signature.asc` | `af8689111ee427d9f78e5bca17d1d2227ae8787071adf8b1ded66e36a6058c0c` |

Never hand-edit these files; change the pins in `docs/VERSIONS.md` and
re-run the script.

| File | sha256 |
|---|---|
| `rfc9580.txt` | `36540e57b63d24c0b17be0748b704bdeed3d643ce99a4fb1abb6469c1eb1ff18` |
| `rfc9980.txt` | `3d2b30bdaecaf28bfde85545bd24e661a97f1e7b371a963778d4314a678c93ac` |
| `draft-dkg-openpgp-stateless-cli-16.txt` | `3545c255c8af4db21757a6838ef06987bc0ec57889aa902f617e01ada5e48c53` |
