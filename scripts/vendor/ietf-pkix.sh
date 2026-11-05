#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# scripts/vendor/ietf-pkix.sh
#
# Vendors the IETF RFCs the revocation check of ferrohealth-server (feature
# `revocation`, #243) implements into docs/specs/ietf-pkix/, each as the
# plain-text RFC the RFC Editor publishes: the X.509 certificate and CRL
# profile (RFC 5280), whose §5 and §6.3 the CRL check follows, and OCSP
# (RFC 6960), whose request and basic response the OCSP check speaks.
#
# An RFC never changes once published, so each "IETF RFC <n>" row of
# docs/VERSIONS.md pins its URL and the sha256 of its bytes; the script fails
# when a download differs.
#
# Redistribution: every RFC is subject to BCP 78, whose IETF Trust Legal
# Provisions grant every person the right to copy, publish, display and
# distribute IETF Documents in full and without modification. The script
# fails if an RFC stops naming BCP 78.
#
# Usage: scripts/vendor/ietf-pkix.sh
# Requires: curl, shasum, awk.
#
# No specification governs this file: our own design.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

die() {
  echo "ietf-pkix: $*" >&2
  exit 1
}

for tool in curl shasum awk; do
  command -v "$tool" > /dev/null 2>&1 || die "$tool is not installed"
done

readonly dest="docs/specs/ietf-pkix"
readonly numbers=(5280 6960)
tick='`'

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pins=""
rows=""
for number in "${numbers[@]}"; do
  item="IETF RFC $number"
  row="$(grep -F "| $item |" docs/VERSIONS.md || true)"
  [[ -n "$row" ]] || die "docs/VERSIONS.md has no '$item' row"
  url="$(grep -oE "${tick}https://[^${tick}]+${tick}" <<< "$row" | head -n1 | tr -d "$tick")"
  want="$(grep -oE '\b[0-9a-f]{64}\b' <<< "$row" | head -n1 || true)"
  [[ -n "$url" ]] && [[ -n "$want" ]] || die "the '$item' row names no URL or sha256"
  echo "ietf-pkix: fetching RFC $number"
  curl -fsSL --proto '=https' -o "$tmp/rfc$number.txt" "$url"
  got="$(shasum -a 256 "$tmp/rfc$number.txt" | awk '{ print $1 }')"
  [[ "$got" = "$want" ]] || die "RFC $number has sha256 $got, and docs/VERSIONS.md pins $want"
  grep -qF "BCP 78" "$tmp/rfc$number.txt" || die "RFC $number does not name BCP 78"
  pins="$pins
- RFC $number: <$url>, sha256 \`$want\`"
  rows="$rows
| \`rfc$number.txt\` | \`$want\` |"
done

rm -rf -- "${dest:?}"
mkdir -p "$dest"
for number in "${numbers[@]}"; do
  cp "$tmp/rfc$number.txt" "$dest/rfc$number.txt"
done

cat > "$dest/PROVENANCE.md" << PROV
<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the IETF PKIX RFCs

- Source: the RFC Editor, <https://www.rfc-editor.org/>, the plain-text
  publication of each RFC
- Pins, one row of \`docs/VERSIONS.md\` per RFC, by URL and sha256:$pins
- Fetched: $(date -u +%Y-%m-%d) by \`scripts/vendor/ietf-pkix.sh\`
- Licence: each RFC is subject to BCP 78. The IETF Trust Legal Provisions
  (<https://trustee.ietf.org/license-info>) grant every person the right to
  copy, publish, display and distribute IETF Documents in full and without
  modification; the files are those documents, in full and unmodified, and
  keep their upstream terms
- Read by: the revocation check of \`ferrohealth-server\` (feature
  \`revocation\`), which cites RFC 5280 §4.2.1.13, §5 and §6.3 for its CRL
  check and RFC 6960 for its OCSP check

Never hand-edit these files; change the pins in \`docs/VERSIONS.md\` and
re-run the script.

| File | sha256 |
|---|---|$rows
PROV
echo "ietf-pkix: vendored ${#numbers[@]} RFCs under $dest"
