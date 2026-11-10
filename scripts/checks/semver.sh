#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
#
# The SemVer judge of the published crates (.claude/rules/crates-publishing.md,
# #349), after FerroBRIDGE's scripts/checks/semver.sh: cargo-semver-checks
# compares the public API of each crate with its newest release on crates.io
# and refuses a break that the version in the manifest does not answer with a
# minor bump while the crate is 0.x (0.x.y to 0.(x+1).0), the Cargo SemVer
# rules of https://doc.rust-lang.org/cargo/reference/semver.html. A version
# equal to the release is judged as a change that breaks nothing. Each crate
# is judged twice: on the judge's default feature heuristic, and with
# --only-explicit-features (no default features), which is what a product
# built with `default-features = false` compiles against. A public dependency
# (scripts/lib/public-deps.sh) moved to another compatibility line
# breaks the crate where the judge cannot see it, so it needs the next minor
# at patch 0. No specification governs where the judge runs: our own design.
# CI runs it on every pull request and in the merge queue, and the release
# plan over the crate a tag releases.
#
#   semver.sh [<crate>...]   judge the named crates, or every crate past
#                            0.0.0
#   semver.sh --changed <base>
#                            judge every crate past 0.0.0 when the change
#                            from <base> to HEAD reaches a crate, and
#                            nothing otherwise
#   semver.sh --pin          print the cargo-semver-checks version pinned in
#                            docs/VERSIONS.md, which CI installs
#
# A change that reaches no crate leaves every public API as it is on its
# base, so judging it would only judge the base: a pull request opened
# before a release and checked after it would be refused for the release's
# own changes. A change reaches the crates when it touches crates/, the
# workspace manifest or lock, the toolchain, the pin or this judge; it then
# judges every crate, since one crate's change can reach another's API
# through a re-export. A crate at 0.0.0, or with no release on crates.io, has
# nothing to compare with and is named as left out.
#
# The installed cargo-semver-checks must be the pinned version. Exit 1 when
# a crate's version does not answer its change, or the judge could not judge
# it (named apart from a break), 2 on a usage or setup error.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
# shellcheck disable=SC1091 # shellcheck is not run with -x; the library is checked on its own
. scripts/lib/public-deps.sh

usage() {
  echo "usage: semver.sh [<crate>...] | --changed <base> | --pin" >&2
  exit 2
}

# The Pin cell of the cargo-semver-checks row, its backticks stripped.
pin="$(awk -F'|' '$2 ~ /^ *cargo-semver-checks *$/ { v = $3; gsub(/[^0-9.]/, "", v); print v; exit }' \
  docs/VERSIONS.md)"
if [[ -z "$pin" ]]; then
  echo "semver: docs/VERSIONS.md pins no cargo-semver-checks version" >&2
  exit 2
fi
if [[ "${1:-}" == "--pin" ]]; then
  [[ "$#" -eq 1 ]] || usage
  echo "$pin"
  exit 0
fi

if ! installed="$(cargo semver-checks --version 2>/dev/null)"; then
  echo "semver: cargo-semver-checks is not installed; install $pin (cargo binstall cargo-semver-checks@$pin)" >&2
  exit 2
fi
if [[ "$installed" != "cargo-semver-checks $pin" ]]; then
  echo "semver: found '$installed', docs/VERSIONS.md pins $pin" >&2
  exit 2
fi

if [[ "${1:-}" == "--changed" ]]; then
  [[ "$#" -eq 2 ]] || usage
  if ! changed="$(git diff --name-only "$2" HEAD)"; then
    echo "semver: cannot compare $2 with HEAD" >&2
    exit 2
  fi
  if ! grep -qE '^(crates/|Cargo\.toml$|Cargo\.lock$|rust-toolchain\.toml$|docs/VERSIONS\.md$|scripts/checks/semver\.sh$|scripts/lib/public-deps\.sh$)' <<<"$changed"; then
    echo "semver: the change from $2 reaches no crate; every public API is its base's"
    exit 0
  fi
  set --
fi

# The `[package]` table's own `version` in the manifest on stdin.
package_version() {
  awk -F'"' '/^\[package\]/{p=1; next} /^\[/{p=0} p && /^version[[:space:]]*=/{print $2; exit}'
}

log="$(mktemp)"
document="$(mktemp)"
trap 'rm -f "$log" "$document"' EXIT

# Reads the crates.io API document at path $1 under /api/v1/crates/ into
# $document and prints the HTTP status, 000 when no answer came. The
# User-Agent names the script and never a person.
crates_io() {
  curl --proto '=https' --tlsv1.2 -sS -o "$document" -w '%{http_code}' \
    -H 'User-Agent: ferrosys-semver (scripts/checks/semver.sh)' \
    "https://crates.io/api/v1/crates/$1" 2>/dev/null || true
}

# Whether $1 and $2 share their compatibility line: the major, and the minor
# too while the major is 0 (the Cargo caret rule).
same_line() {
  [[ "$(compat_line "$1")" == "$(compat_line "$2")" ]]
}

# Judges crate $1 at $2 against its release $3 in one feature pass, named $4,
# with the extra arguments given to the judge. Only a summary the judge
# printed counts as a judgement: a judge that could not build is named apart
# from a break.
judge() {
  local crate="$1" current="$2" previous="$3" pass="$4" status=0
  shift 4
  echo "semver: $crate $previous -> $current, $pass"
  cargo semver-checks check-release --package "$crate" "$@" > "$log" 2>&1 || status=$?
  cat "$log"
  if [[ "$status" -ne 0 ]] && grep -q 'semver requires new' "$log"; then
    echo "::error::semver: $crate $current breaks its public API against $previous ($pass); a breaking change bumps the minor while the crate is 0.x (.claude/rules/crates-publishing.md)." >&2
    return 1
  fi
  if [[ "$status" -ne 0 ]] || ! grep -q 'Summary' "$log"; then
    echo "::error::semver: cargo-semver-checks could not judge $crate $current against $previous ($pass, exit $status); this is no SemVer verdict, see its output above." >&2
    return 1
  fi
}

crates=("$@")
if [[ "${#crates[@]}" -eq 0 ]]; then
  for manifest in crates/*/Cargo.toml; do
    crates+=("$(basename "$(dirname "$manifest")")")
  done
fi

failed=""
for crate in "${crates[@]}"; do
  [[ -f "crates/$crate/Cargo.toml" ]] || { echo "semver: no crate crates/$crate" >&2; exit 2; }
  current="$(package_version < "crates/$crate/Cargo.toml")"
  if [[ "$current" == "0.0.0" ]]; then
    echo "semver: $crate is at 0.0.0, with no release to compare with; left out"
    continue
  fi
  status="$(crates_io "$crate")"
  previous=""
  case "$status" in
    200) previous="$(jq -r '.crate.max_stable_version // empty' "$document")" ;;
    404) ;;
    *)
      echo "semver: crates.io answered $status for $crate" >&2
      exit 2
      ;;
  esac
  if [[ -z "$previous" ]] || [[ "$previous" == "0.0.0" ]]; then
    echo "semver: $crate has no release on crates.io to compare with; left out"
    continue
  fi
  verdict=0
  judge "$crate" "$current" "$previous" "the default feature heuristic" || verdict=1
  judge "$crate" "$current" "$previous" "no default features" --only-explicit-features || verdict=1
  # A public dependency on another line breaks the crate where the judge
  # cannot see it: it needs the next minor, at patch 0.
  status="$(crates_io "$crate/$previous/dependencies")"
  if [[ "$status" != "200" ]]; then
    echo "semver: crates.io answered $status for the dependencies of $crate $previous" >&2
    exit 2
  fi
  moved="$(moved_lines \
    "$(jq -r '.dependencies[] | select(.kind == "normal") | "\(.crate_id) \(.req)"' "$document" | public_lines)" \
    "$(manifest_requirements Cargo.toml "crates/$crate/Cargo.toml" | public_lines)")"
  if [[ -n "$moved" ]]; then
    if same_line "$previous" "$current" || [[ "${current##*.}" != "0" ]]; then
      echo "::error::semver: $crate moves from $previous to $current while a public dependency moves to another compatibility line ($moved); that breaks its consumers, so it takes the next minor at patch 0 (.claude/rules/crates-publishing.md)." >&2
      verdict=1
    else
      echo "semver: $crate leaves its line for its public dependencies ($moved)"
    fi
  fi
  [[ "$verdict" -eq 0 ]] || failed="$failed $crate"
done

if [[ -n "$failed" ]]; then
  echo "semver: refused:$failed" >&2
  exit 1
fi
echo "semver: every judged version answers its change"
