#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# The post-quantum inventory gate (no specification governs it: our own
# design). conformance/post-quantum.tsv lists every cryptographic use in five
# tab-separated columns, which scripts/checks/badge-logos/logo.sh (vendored
# from FerroSMART) describes. The gate holds every repository path a location
# names to the tree, the table of docs/post-quantum.md to the file, and the
# shields.io endpoint badge conformance/badges/post-quantum.json, which the
# README reads, to the count the shared helper badge_post_quantum draws and
# to the shared length limits of badge_length_check.
#
#   scripts/checks/post-quantum.sh             check; warn on drift
#   scripts/checks/post-quantum.sh --update    rewrite the table and the badge
#   scripts/checks/post-quantum.sh --check     fail on any drift (CI)
#   scripts/checks/post-quantum.sh --self-test prove the rules on fixtures
#
# Needs jq. Exit 0 when clean, 1 on a defect or (under --check) drift, 2 on a
# usage error.
set -euo pipefail

# shellcheck source=scripts/lib/guard-args.sh
. "$(dirname "$0")/../lib/guard-args.sh"
# shellcheck source=scripts/checks/badge-logos/logo.sh
. "$(dirname "$0")/badge-logos/logo.sh"

readonly GRAMMAR="[--update | --check | --self-test]"
readonly INVENTORY=conformance/post-quantum.tsv
readonly PAGE=docs/post-quantum.md
readonly BADGE=conformance/badges/post-quantum.json

# The data rows of inventory $1, without its comments and its header.
rows_of() {
  grep -vE '^(#|$)' "$1" | tail -n +2
}

# Fails for each repository path a location of inventory $1 names that the
# tree $2 does not hold. A location is a comma-separated list; a path is the
# part of an item before " (", and only an item with a "/" is a path.
locations_of() {
  local inventory="$1" tree="$2" failed=0 location rest item path
  while IFS=$'\t' read -r _ _ _ _ location; do
    rest="$location"
    while [[ -n "$rest" ]]; do
      item="${rest%%, *}"
      if [[ "$rest" == *", "* ]]; then rest="${rest#*, }"; else rest=""; fi
      path="${item%% (*}"
      if [[ "$path" == */* && "$path" != *" "* && ! -e "$tree/$path" ]]; then
        echo "post-quantum: $inventory names $path, which the tree does not hold" >&2
        failed=1
      fi
    done
  done < <(rows_of "$inventory")
  return "$failed"
}

# The markdown table inventory $1 implies.
table_of() {
  echo '| Use | Algorithm | Safe | Post-quantum standard | Location |'
  echo '|---|---|---|---|---|'
  rows_of "$1" | awk -F'\t' '{ printf "| %s | %s | %s | %s | `%s` |\n", $1, $2, $3, $4, $5 }'
}

# The table of page $1: the lines from its header row to the first line that
# is no table row.
page_table() {
  awk '/^\| Use \| Algorithm \|/ { inside = 1 } inside && !/^\|/ { exit } inside { print }' "$1"
}

# Rewrites the table of page $1 from inventory $2 under update, else
# compares; status 1 under check when they disagree.
emit_table() {
  local page="$1" inventory="$2" mode="$3" table rendered
  table="$(table_of "$inventory")"
  if [[ -z "$(page_table "$page")" ]]; then
    echo "post-quantum: $page has no inventory table" >&2
    return 1
  fi
  if [[ "$mode" = update ]]; then
    rendered="$(mktemp)"
    printf '%s\n' "$table" >"$rendered"
    awk -v table="$rendered" '
      /^\| Use \| Algorithm \|/ && !done { while ((getline line < table) > 0) print line; skipping = 1; done = 1; next }
      skipping && /^\|/ { next }
      { skipping = 0; print }
    ' "$page" >"$rendered.page"
    cat "$rendered.page" >"$page"
    rm -f "$rendered" "$rendered.page"
  elif [[ "$(page_table "$page")" != "$table" ]]; then
    echo "post-quantum: the $page table disagrees with $inventory; run $0 --update"
    if [[ "$mode" = check ]]; then return 1; fi
  fi
  return 0
}

# Writes badge file $1 from inventory $2 under update, else compares; status
# 1 under check when they disagree.
emit_badge() {
  local badge="$1" inventory="$2" mode="$3" json
  json="$(badge_post_quantum "$inventory")" || return 1
  if [[ "$mode" = update ]]; then
    mkdir -p "$(dirname "$badge")"
    printf '%s\n' "$json" >"$badge"
  elif [[ ! -f "$badge" ]] || ! badge_post_quantum_check "$inventory" "$badge"; then
    echo "post-quantum: $badge disagrees with $inventory; run $0 --update"
    if [[ "$mode" = check ]]; then return 1; fi
  fi
  return 0
}

self_test() {
  local work failed=0
  work="$(mktemp -d)"
  # shellcheck disable=SC2064 # expand $work now: the local is gone at EXIT.
  trap "rm -rf '$work'" EXIT
  # expect WANT NAME COMMAND...: COMMAND exits WANT.
  expect() {
    local want="$1" name="$2" status=0
    shift 2
    "$@" >/dev/null 2>&1 || status=$?
    if [[ "$status" -ne "$want" ]]; then
      echo "post-quantum: self-test failed: $name exited $status, wanted $want" >&2
      failed=1
    fi
  }
  badge_post_quantum_self_test >/dev/null || failed=1
  mkdir -p "$work/tree/src"
  : >"$work/tree/src/kx.rs"
  {
    printf '# SPDX-FileCopyrightText: Cadasto B.V.\n'
    printf '%s\n' "$badge_post_quantum_header"
    printf 'Key exchange\tX25519MLKEM768\tyes\tRFC 10024\tsrc/kx.rs (provider), Cargo.toml (rustls features)\n'
    printf 'Tokens\tES256\tno\tRFC 9964\ta-crate resource\n'
  } >"$work/inventory.tsv"
  expect 0 "every location in the tree" locations_of "$work/inventory.tsv" "$work/tree"
  sed 's#src/kx.rs#src/gone.rs#' "$work/inventory.tsv" >"$work/missing.tsv"
  expect 1 "a location the tree does not hold" locations_of "$work/missing.tsv" "$work/tree"
  expect 1 "a missing badge under check" emit_badge "$work/badges/pq.json" "$work/inventory.tsv" check
  expect 0 "a badge written" emit_badge "$work/badges/pq.json" "$work/inventory.tsv" update
  expect 0 "a written badge under check" emit_badge "$work/badges/pq.json" "$work/inventory.tsv" check
  if [[ "$(jq -r '.message + " " + .color' "$work/badges/pq.json")" != "1/2 yellow" ]]; then
    echo "post-quantum: self-test failed: the badge of one safe row of two is $(cat "$work/badges/pq.json")" >&2
    failed=1
  fi
  expect 0 "the badge carries a logo" badge_logo_check "$work/badges/pq.json"
  expect 0 "the badge fits the length limits" badge_length_check "$work/badges/pq.json"
  jq -c '.label = "post-quantum safe cryptographic uses"' "$work/badges/pq.json" >"$work/long.json"
  expect 1 "a label past the limit" badge_length_check "$work/long.json"
  badge_length_self_test >/dev/null || failed=1
  sed $'s#\tno\t#\tyes\t#' "$work/inventory.tsv" >"$work/moved.tsv"
  expect 1 "a badge whose count moved under check" emit_badge "$work/badges/pq.json" "$work/moved.tsv" check
  printf '# Page\n\nintro\n\n| Use | Algorithm | Safe | Post-quantum standard | Location |\n|---|---|---|---|---|\n| stale | x | no | y | z |\n\nrest\n' >"$work/page.md"
  expect 1 "a stale table under check" emit_table "$work/page.md" "$work/inventory.tsv" check
  expect 0 "a stale table under compare" emit_table "$work/page.md" "$work/inventory.tsv" compare
  expect 0 "the table rewritten" emit_table "$work/page.md" "$work/inventory.tsv" update
  expect 0 "a rewritten table under check" emit_table "$work/page.md" "$work/inventory.tsv" check
  if [[ "$(head -n 3 "$work/page.md" | tail -n 1)" != intro || "$(tail -n 1 "$work/page.md")" != rest ]]; then
    echo "post-quantum: self-test failed: the rewrite touched text outside the table" >&2
    failed=1
  fi
  printf '# Page\n' >"$work/bare.md"
  expect 1 "a page without a table" emit_table "$work/bare.md" "$work/inventory.tsv" check
  if [[ "$failed" -eq 0 ]]; then
    echo "post-quantum: self-test OK"
  fi
  return "$failed"
}

mode=compare
case "${1-}" in
  "") ;;
  --update) mode=update ;;
  --check) mode=check ;;
  --self-test)
    [[ "$#" -eq 1 ]] || guard_usage "$GRAMMAR"
    self_test
    exit $?
    ;;
  *) guard_usage "$GRAMMAR" ;;
esac
[[ "$#" -le 1 ]] || guard_usage "$GRAMMAR"
cd "$(dirname "$0")/../.."

failed=0
locations_of "$INVENTORY" . || failed=1
emit_table "$PAGE" "$INVENTORY" "$mode" || failed=1
emit_badge "$BADGE" "$INVENTORY" "$mode" || failed=1
badge_length_check "$BADGE" || failed=1
if [[ "$failed" -eq 0 ]]; then
  echo "post-quantum: $(jq -r .message "$BADGE") uses safe"
fi
exit "$failed"
