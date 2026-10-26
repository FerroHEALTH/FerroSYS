#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# The licence guard: no first-party file claims a licence other than the
# project's, BUSL-1.1, in its SPDX header. LICENSE itself names the Change
# License, and vendored trees under docs/specs/ keep their upstream terms, so
# both are left out (no specification governs this: our own design).
#
# Usage: scripts/checks/licence-headers.sh
set -euo pipefail
cd "$(dirname "$0")/../.."

licence="BUSL-1.1"
failures=0
while IFS= read -r path; do
  [[ -f "$path" ]] || continue
  claimed="$(grep -m1 -oE 'SPDX-License-Identifier: [A-Za-z0-9.+-]+' "$path" | cut -d' ' -f2 || true)"
  if [[ -n "$claimed" && "$claimed" != "$licence" ]]; then
    echo "licence-headers: $path claims $claimed, the project licence is $licence" >&2
    failures=$((failures + 1))
  fi
done < <(git ls-files | grep -v '^docs/specs/' | grep -v '^LICENSE$')

if [[ "$failures" -gt 0 ]]; then
  exit 1
fi
echo "licence-headers: every first-party SPDX header names $licence"
