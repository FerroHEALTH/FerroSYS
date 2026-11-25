#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# The VEX drift gate: the published OpenVEX document must be exactly what the
# generator produces from `deny.toml` + `security/vex/rust-advisories.toml`.
#
# Two failure modes it exists to catch, both of which have precedent in
# FerroEHR's history of second lists that nothing read:
#
#   1. An advisory is accepted in `deny.toml` and no justification is published,
#      so a downstream scanner sees an unexplained finding while the argument
#      sits in a TOML comment nobody downstream reads. (The generator refuses;
#      this gate is where that refusal reaches CI.)
#   2. The JSON is hand-edited, or the prose changes without regeneration, and
#      the published document quietly stops matching the gate it claims to
#      describe.
#
# Mutation-proven in both directions: adding an ignore to `deny.toml` without a
# prose entry fails, and touching one byte of the generated JSON fails.
#
# What this gate CANNOT see: whether either side still describes reality. An
# ignore and its justification agree perfectly right up until a dependency
# upgrade resolves the advisory, at which point both are stale and both still
# pass here. `scripts/checks/advisory-exceptions.sh` is that half, and it runs in
# the cargo-deny job, which is where the graph is resolvable.
#
# A document with no statement is valid: it says no advisory is accepted,
# which is the truth while deny.toml ignores none, and the release path keeps
# one fixed file to publish. Its `statements` must still be an array.
#
# Usage: scripts/checks/vex-advisories.sh [--self-test]
# VEX_DIR moves the VEX directory (default security/vex) and VEX_GATE the
# gate file (default deny.toml); the generator reads the same variables.
set -euo pipefail

# shellcheck source=scripts/lib/guard-args.sh
. "$(dirname "$0")/../lib/guard-args.sh"
self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
cd "$(dirname "$0")/../.."

# Runs the guard over fixtures in a directory of their own: the empty
# document passes; a statement with no justification, an advisory the gate
# ignores with no statement, a statement for one it does not ignore and a
# document whose statements are no array each fail.
self_test() {
  local dir status
  dir="$(mktemp -d)"
  trap 'rm -rf "$dir"' RETURN
  mkdir -p "$dir/empty"
  cat > "$dir/empty/rust-advisories.toml" <<'TOML'
[document]
id = "https://example.org/vex.json"
author = "Self-test"
role = "vendor"
timestamp = "2026-01-01T00:00:00Z"
version = 1
products = ["pkg:cargo/ferrohealth-openpgp-pq"]
TOML
  printf '[advisories]\nversion = 2\nignore = []\n' > "$dir/empty/deny.toml"
  VEX_DIR="$dir/empty" VEX_GATE="$dir/empty/deny.toml" bash scripts/security/vex-generate.sh > /dev/null
  run() {
    local case_name="$1"
    status=0
    VEX_DIR="$dir/$case_name" VEX_GATE="$dir/$case_name/deny.toml" bash "$self" > "$dir/$case_name.out" 2>&1 || status=$?
  }
  run empty
  [[ "$status" -eq 0 ]] || { cat "$dir/empty.out" >&2; echo "self-test: the empty document was refused" >&2; return 1; }
  cp -R "$dir/empty" "$dir/unjustified"
  printf '%s\n' '{"@context":"https://openvex.dev/ns/v0.2.0","@id":"x","author":"a","timestamp":"t","version":1,"statements":[{"vulnerability":{"name":"RUSTSEC-0000-0000"},"products":[],"status":"not_affected"}]}' \
    > "$dir/unjustified/layers.openvex.json"
  run unjustified
  [[ "$status" -ne 0 ]] || { echo "self-test: a statement with no justification passed" >&2; return 1; }
  cp -R "$dir/empty" "$dir/unstated"
  printf '[advisories]\nversion = 2\nignore = [\n  { id = "RUSTSEC-0000-0000", reason = "self-test" },\n]\n' > "$dir/unstated/deny.toml"
  run unstated
  [[ "$status" -ne 0 ]] || { echo "self-test: an ignored advisory with no statement passed" >&2; return 1; }
  cp -R "$dir/empty" "$dir/stated"
  cat >> "$dir/stated/rust-advisories.toml" <<'TOML'

[[accepted]]
id = "RUSTSEC-0000-0000"
crate = "none"
carriers = []
workspace_roots = []
status = "not_affected"
justification = "component_not_present"
impact = "Self-test."
TOML
  run stated
  [[ "$status" -ne 0 ]] || { echo "self-test: a statement for an advisory the gate does not ignore passed" >&2; return 1; }
  cp -R "$dir/empty" "$dir/scalar"
  jq '.statements = {}' "$dir/empty/rust-advisories.openvex.json" > "$dir/scalar/other.openvex.json"
  run scalar
  [[ "$status" -ne 0 ]] || { echo "self-test: a document whose statements are no array passed" >&2; return 1; }
  echo "vex-advisories: self-test passed"
}

case "${1:-}" in
  --self-test)
    self_test
    exit $?
    ;;
  *) guard_no_args "$@" ;;
esac

readonly VEX_DIR="${VEX_DIR:-security/vex}"
export VEX_DIR
readonly OUT="$VEX_DIR/rust-advisories.openvex.json"

[[ -f "$OUT" ]] || {
  echo "error: $OUT does not exist: run scripts/security/vex-generate.sh" >&2
  exit 1
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The generator carries the id-set agreement and the OpenVEX vocabulary checks
# and exits non-zero on any of them, so running it IS half this gate.
bash scripts/security/vex-generate.sh --stdout > "$work/expected.json"

if ! diff -u "$OUT" "$work/expected.json" > "$work/diff.txt"; then
  echo "error: $OUT is not what the generator produces" >&2
  echo >&2
  sed 's/^/  /' "$work/diff.txt" >&2
  echo >&2
  echo "The document is GENERATED: never hand-edit it. Change deny.toml or" >&2
  echo "$VEX_DIR/rust-advisories.toml, then run:" >&2
  echo "  bash scripts/security/vex-generate.sh" >&2
  exit 1
fi

# Every document under security/vex/ has to be well-formed OpenVEX, so a typo
# does not ship as a document a consumer's parser rejects; the others are
# hand-authored (they describe inherited container layers, where nothing in
# this repository resolves the finding).
docs=("$VEX_DIR"/*.openvex.json)
for doc in "${docs[@]}"; do
  # The generated document may hold no statement; a hand-authored one exists
  # to state a finding, so it holds at least one.
  least=1
  [[ "$doc" = "$OUT" ]] && least=0
  jq -e --argjson least "$least" '
    (has("@context") and has("@id") and has("author") and has("timestamp")
     and has("version") and (.statements | type == "array" and length >= $least))
    and (.statements | all(
      has("vulnerability") and has("products") and has("status")
      and (.status | IN("not_affected", "affected", "fixed", "under_investigation"))
      and (if .status == "not_affected" then
             (.justification | IN(
               "component_not_present", "vulnerable_code_not_present",
               "vulnerable_code_not_in_execute_path",
               "vulnerable_code_cannot_be_controlled_by_adversary",
               "inline_mitigations_already_exist"))
           else true end)))
  ' "$doc" > /dev/null || {
    echo "error: $doc is not a well-formed OpenVEX document" >&2
    echo "  (https://github.com/openvex/spec/blob/main/OPENVEX-SPEC.md)" >&2
    exit 1
  }
done

echo "ok: ${#docs[@]} VEX documents well-formed; $OUT matches deny.toml"
