#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# scripts/vendor/ietf-openpgp.sh
#
# Vendors the IETF documents ferrohealth-openpgp-pq implements into
# docs/specs/ietf-openpgp/, each as the plain text the IETF publishes:
# OpenPGP (RFC 9580), post-quantum cryptography in OpenPGP (RFC 9980), and
# the Stateless OpenPGP command line draft that `ferrohealth-sop` follows.
# It also extracts the RFC 9980 Appendix A.3 ML-DSA-65+Ed25519 vectors, the
# transferable secret key (A.3.1), the transferable public key (A.3.2) and
# the detached signature (A.3.4), verbatim into
# crates/ferrohealth-openpgp-pq/tests/vectors/, so the published crate's
# tests carry them.
#
# A published RFC and a numbered draft revision never change, so each row of
# docs/VERSIONS.md pins its URL and the sha256 of its bytes; the script fails
# when a download differs.
#
# Redistribution: every RFC and Internet-Draft is subject to BCP 78, whose
# IETF Trust Legal Provisions grant every person the right to copy, publish,
# display and distribute IETF Documents in full and without modification,
# and to extract code components such as the armored test vectors. The
# script fails if a document stops naming BCP 78.
#
# Usage: scripts/vendor/ietf-openpgp.sh
# Requires: curl, shasum, awk.
#
# No specification governs this file: our own design.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

die() {
  echo "ietf-openpgp: $*" >&2
  exit 1
}

for tool in curl shasum awk; do
  command -v "$tool" > /dev/null 2>&1 || die "$tool is not installed"
done

readonly dest="docs/specs/ietf-openpgp"
readonly vectors="crates/ferrohealth-openpgp-pq/tests/vectors"
# The docs/VERSIONS.md row of each document, then the file it is stored as.
readonly items=(
  "IETF RFC 9580|rfc9580.txt"
  "IETF RFC 9980|rfc9980.txt"
  "IETF draft-dkg-openpgp-stateless-cli-16|draft-dkg-openpgp-stateless-cli-16.txt"
)
tick='`'

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pins=""
rows=""
for entry in "${items[@]}"; do
  item="${entry%%|*}"
  file="${entry#*|}"
  row="$(grep -F "| $item |" docs/VERSIONS.md || true)"
  [[ -n "$row" ]] || die "docs/VERSIONS.md has no '$item' row"
  url="$(grep -oE "${tick}https://[^${tick}]+${tick}" <<< "$row" | head -n1 | tr -d "$tick")"
  want="$(grep -oE '\b[0-9a-f]{64}\b' <<< "$row" | head -n1 || true)"
  [[ -n "$url" ]] && [[ -n "$want" ]] || die "the '$item' row names no URL or sha256"
  echo "ietf-openpgp: fetching $file"
  curl -fsSL --proto '=https' -o "$tmp/$file" "$url"
  got="$(shasum -a 256 "$tmp/$file" | awk '{ print $1 }')"
  [[ "$got" = "$want" ]] || die "$file has sha256 $got, and docs/VERSIONS.md pins $want"
  grep -qF "BCP 78" "$tmp/$file" || die "$file does not name BCP 78"
  pins="$pins
- $item: <$url>, sha256 \`$want\`"
  rows="$rows
| \`$file\` | \`$want\` |"
done

# Prints the armored block that follows the heading line $2 in $1, without
# the three-space indent of the RFC text.
extract() {
  awk -v heading="$2" '
    $0 == heading { found = 1; next }
    found && /-----BEGIN PGP/ { inside = 1 }
    inside { sub(/^   /, ""); print }
    inside && /-----END PGP/ { exit }
  ' "$1"
}

mkdir -p "$tmp/vectors"
extract "$tmp/rfc9980.txt" "A.3.1.  Transferable Secret Key" > "$tmp/vectors/rfc9980-a3.1-secret-key.asc"
extract "$tmp/rfc9980.txt" "A.3.2.  Transferable Public Key" > "$tmp/vectors/rfc9980-a3.2-certificate.asc"
extract "$tmp/rfc9980.txt" "A.3.4.  Detached Signature" > "$tmp/vectors/rfc9980-a3.4-signature.asc"
vector_rows=""
for vector in "$tmp"/vectors/*.asc; do
  [[ -s "$vector" ]] || die "no armored block found for $(basename "$vector")"
  vector_rows="$vector_rows
| \`$(basename "$vector")\` | \`$(shasum -a 256 "$vector" | awk '{ print $1 }')\` |"
done

rm -rf -- "${dest:?}"
mkdir -p "$dest" "$vectors"
for entry in "${items[@]}"; do
  file="${entry#*|}"
  cp "$tmp/$file" "$dest/$file"
done
cp "$tmp"/vectors/*.asc "$vectors/"

cat > "$dest/PROVENANCE.md" << PROV
<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the IETF OpenPGP documents

- Source: the RFC Editor, <https://www.rfc-editor.org/>, and the IETF
  Internet-Draft archive, <https://www.ietf.org/archive/id/>, the plain-text
  publication of each document
- Pins, one row of \`docs/VERSIONS.md\` per document, by URL and sha256:$pins
- Fetched: $(date -u +%Y-%m-%d) by \`scripts/vendor/ietf-openpgp.sh\`
- Licence: each document is subject to BCP 78. The IETF Trust Legal
  Provisions (<https://trustee.ietf.org/license-info>) grant every person
  the right to copy, publish, display and distribute IETF Documents in full
  and without modification; the files are those documents, in full and
  unmodified, and keep their upstream terms
- Read by: \`ferrohealth-openpgp-pq\`, which cites RFC 9580 and RFC 9980 for
  its keys and signatures, and whose \`ferrohealth-sop\` binary follows the
  Stateless OpenPGP draft at revision 16

The RFC 9980 Appendix A.3 vectors are extracted verbatim into
\`$vectors/\`, where the crate's tests read them:

| File | sha256 |
|---|---|$vector_rows

Never hand-edit these files; change the pins in \`docs/VERSIONS.md\` and
re-run the script.

| File | sha256 |
|---|---|$rows
PROV
echo "ietf-openpgp: vendored ${#items[@]} documents under $dest and the A.3 vectors under $vectors"
