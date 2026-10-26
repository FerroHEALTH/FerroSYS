#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# scripts/vendor/otel-semconv.sh
#
# Vendors the pages of the OpenTelemetry semantic conventions that
# ferrohealth-telemetry cites: the HTTP metrics and spans, the service and
# deployment resource conventions, and the attribute registry pages they
# draw on, with the licence. It reads the pin from the "OpenTelemetry
# semantic conventions" row of docs/VERSIONS.md (a release tag and its
# commit), copies the files verbatim into docs/specs/otel-semconv/ at their
# upstream paths, and writes a PROVENANCE.md with the sha256 of each.
# OpenTelemetry owns the conventions; never hand-edit the copy, move the pin
# and re-run.
#
# Usage: scripts/vendor/otel-semconv.sh
# Requires: curl, shasum, awk.
#
# No specification governs this file: our own design.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

die() {
  echo "otel-semconv: $*" >&2
  exit 1
}

for tool in curl shasum awk; do
  command -v "$tool" > /dev/null 2>&1 || die "$tool is not installed"
done

item="OpenTelemetry semantic conventions"
dest="docs/specs/otel-semconv"
files=(
  LICENSE
  docs/http/http-metrics.md
  docs/http/http-spans.md
  docs/resource/service.md
  docs/resource/deployment-environment.md
  docs/registry/attributes/deployment.md
  docs/registry/attributes/error.md
  docs/registry/attributes/http.md
  docs/registry/attributes/network.md
  docs/registry/attributes/service.md
  docs/registry/attributes/url.md
)

row="$(grep -F "| $item |" docs/VERSIONS.md || true)"
[ -n "$row" ] || die "docs/VERSIONS.md has no '$item' row"
tick='`'
repo="$(grep -oE "${tick}[A-Za-z0-9-]+/[A-Za-z0-9._-]+${tick}" <<< "$row" | head -n1 | tr -d "$tick")"
tag="$(grep -oE "${tick}v[0-9]+\.[0-9]+\.[0-9]+${tick}" <<< "$row" | head -n1 | tr -d "$tick")"
sha="$(grep -oE '\b[0-9a-f]{40}\b' <<< "$row" | head -n1 || true)"
[ -n "$repo" ] && [ -n "$tag" ] && [ -n "$sha" ] ||
  die "the '$item' row names no repository, release tag or commit"

echo "otel-semconv: $repo $tag at $sha"
rm -rf -- "${dest:?}/docs" "$dest/LICENSE"
digests=""
for path in "${files[@]}"; do
  mkdir -p "$(dirname "$dest/$path")"
  curl --fail --silent --show-error --location \
    "https://raw.githubusercontent.com/$repo/$sha/$path" -o "$dest/$path" ||
    die "$path could not be fetched at $sha"
  digest="$(shasum -a 256 "$dest/$path" | awk '{ print $1 }')"
  digests="$digests- \`$path\`: \`$digest\`"$'\n'
done

cat > "$dest/PROVENANCE.md" << PROV
<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the OpenTelemetry semantic conventions

- Source: <https://github.com/$repo>, release $tag
- Commit: $sha
- Fetched: $(date -u +%Y-%m-%d) by \`scripts/vendor/otel-semconv.sh\`
- Licence: Apache License 2.0, upstream's \`LICENSE\`, copied beside the
  pages; the pages keep their upstream terms
- Contents: ${#files[@]} files at their upstream paths, copied verbatim

${digests}
ferrohealth-telemetry cites these pages for the resource (\`service.*\`,
\`deployment.environment.name\`), the request span and the
\`http.server.request.duration\` histogram. Never hand-edit these files;
change the pin in \`docs/VERSIONS.md\` and re-run the script.
PROV
echo "otel-semconv: vendored ${#files[@]} files under $dest"
