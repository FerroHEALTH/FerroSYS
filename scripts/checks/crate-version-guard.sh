#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# The crate version rule. Every crate under crates/ keeps its own version in
# its own manifest, released by the tag `<crate>-v<version>`. For each one:
# the version never moves backwards, Cargo.lock carries it, and once it moves
# past the 0.0.0 name holder crates/<crate>/CHANGELOG.md has a section for
# it. A change that moves a version is a release change and moves all three
# together (no specification governs this: our own design).
#
# Usage: scripts/checks/crate-version-guard.sh <base-ref> [head-ref]
set -euo pipefail
cd "$(dirname "$0")/../.."

base="${1:?usage: crate-version-guard.sh <base-ref> [head-ref]}"
head="${2:-HEAD}"

# Each reader consumes all of stdin: one that exits early makes `git show`
# die of SIGPIPE on a file larger than the pipe buffer, which pipefail turns
# into a failure of the guard.

# The `version` key of `[package]` in a Cargo.toml read on stdin.
package_version() {
  awk '/^\[package\]/{p=1; next} /^\[/{p=0}
       !done && p && /^version[[:space:]]*=/{gsub(/.*=[[:space:]]*"|".*/, ""); print; done=1}'
}

# The version of the package $1 in a Cargo.lock read on stdin.
locked_version() {
  awk -v name="$1" '$0 == "name = \"" name "\"" {p=1; next}
       !done && p && /^version = /{gsub(/version = "|"/, ""); print; done=1}'
}

failures=0
fail() {
  echo "crate-version-guard: $*" >&2
  failures=$((failures + 1))
}

checked=""
while IFS= read -r manifest; do
  crate="${manifest#crates/}"
  crate="${crate%/Cargo.toml}"
  want="$(git show "$head:$manifest" | package_version)"
  if [[ -z "$want" ]]; then
    fail "$manifest names no version"
    continue
  fi
  # A crate the base does not have yet starts its version line here.
  was="$(git show "$base:$manifest" 2> /dev/null | package_version || true)"
  if [[ -n "$was" && "$was" != "$want" ]]; then
    newest="$(printf '%s\n%s\n' "$was" "$want" | sort -V | tail -1)"
    [[ "$newest" == "$want" ]] || fail "$crate moves backwards, $was to $want"
  fi
  locked="$(git show "$head:Cargo.lock" | locked_version "$crate")"
  [[ "$locked" == "$want" ]] ||
    fail "Cargo.lock holds $crate ${locked:-<none>}, its manifest $want; run cargo update -w"
  changelog="crates/$crate/CHANGELOG.md"
  if [[ "$want" != "0.0.0" ]] && ! git show "$head:$changelog" 2> /dev/null | grep -F "## [$want]" > /dev/null; then
    fail "$changelog has no section for $want"
  fi
  checked="$checked $crate $want;"
done < <(git ls-tree -r --name-only "$head" crates | grep -E '^crates/[^/]+/Cargo\.toml$')

if [[ "$failures" -gt 0 ]]; then
  exit 1
fi
echo "crate-version-guard: every version, the lock and the changelog agree:${checked:- no crate}"
