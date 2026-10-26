#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# Stale pending-work guard. A `TODO(#N):` comment promises that issue N
# carries the missing work; when N closes without it, the pointer lies. This
# guard collects every such number from the tracked tree and fails when the
# tracker reports one closed or missing (no specification governs this: our
# own design).
#
#   scripts/checks/todo-issues.sh
#
# It needs a `gh` that can read the repository's issues: locally your login,
# in CI the job's GITHUB_TOKEN with `issues: read`. When `gh` is absent,
# unauthenticated or offline, the guard says so and exits 0; CI is where it
# gates.
set -euo pipefail
cd "$(dirname "$0")/../.."

# One "number<TAB>file:line" record per pointer.
pointers() {
  # git grep exits 1 when nothing matches, which is the clean tree.
  { git grep -nE 'TODO\(#[0-9]+\)' -- . || true; } |
    while IFS= read -r hit; do
      local location=${hit%%:*}
      local rest=${hit#*:}
      location="$location:${rest%%:*}"
      grep -oE 'TODO\(#[0-9]+\)' <<< "$hit" | tr -dc '0-9\n' |
        while IFS= read -r number; do
          printf '%s\t%s\n' "$number" "$location"
        done
    done
}

if ! command -v gh > /dev/null 2>&1; then
  echo "todo-issues: skipped: gh is not installed, so the tracker cannot be read."
  exit 0
fi
if ! gh repo view --json name > /dev/null 2>&1; then
  echo "todo-issues: skipped: gh cannot read the repository (unauthenticated or offline)."
  exit 0
fi

records=$(pointers | sort -t $'\t' -k1,1n -k2,2)
if [ -z "$records" ]; then
  echo "todo-issues: no pending-work pointers in the tree."
  exit 0
fi

fail=0
checked=0
while IFS= read -r number; do
  checked=$((checked + 1))
  if ! state=$(gh issue view "$number" --json state --jq .state 2> /dev/null < /dev/null); then
    state=UNKNOWN
  fi
  [ "$state" = OPEN ] && continue
  fail=1
  while IFS=$'\t' read -r n location; do
    [ "$n" = "$number" ] || continue
    echo "::error file=${location%%:*},line=${location##*:}::$location names #$number, which the tracker does not answer as an open issue ($state)."
  done <<< "$records"
done < <(cut -f1 <<< "$records" | sort -un)

if [ "$fail" -eq 0 ]; then
  echo "todo-issues: $checked issue(s) named by pending-work pointers, all open."
fi
exit "$fail"
