#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# Claude Code PostToolUse hook (matcher: Write|Edit).
#
# For an edited .rs file of this checkout, run the per-edit ratchets of
# .claude/rules/rust-style.md: the file-size guard, so a listed file never
# grows and no other file passes the budget, and, for a file under
# crates/*/src or tools/*/src, the forwarder guard, so the file carries no
# forwarder the baseline lacks (it builds tools/forwarders in the
# checkout's target/ on first use, a few seconds). A finding exits 2,
# which feeds it back as a correction. The duplication ratchet runs in CI
# only: a move copies code before it deletes the original, so a per-edit run
# would fire in the middle of every restructure.
#
# No specification governs this: our own design.
set -uo pipefail

payload="$(cat)" || true
if command -v jq >/dev/null 2>&1; then
  file_path="$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null)" || true
else
  file_path="$(printf '%s' "$payload" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
fi

case "${file_path:-}" in
*.rs) ;;
*) exit 0 ;;
esac
[[ -f "$file_path" ]] || exit 0

# The checkout the file belongs to measures it, a worktree included.
repo_root="$(git -C "$(dirname "$file_path")" rev-parse --show-toplevel 2>/dev/null)" || exit 0
[[ -x "$repo_root/scripts/checks/file-size.sh" ]] || exit 0
real_file="$(cd "$(dirname "$file_path")" && pwd -P)/$(basename "$file_path")"
relative="${real_file#"$(cd "$repo_root" && pwd -P)"/}"

findings="$(cd "$repo_root" && bash scripts/checks/file-size.sh --files "$relative" 2>&1)" || {
  printf '%s\n' "$findings" >&2
  exit 2
}

case "$relative" in
crates/*/src/*.rs | tools/*/src/*.rs)
  findings="$(cd "$repo_root" && bash scripts/checks/forwarders.sh --files "$relative" 2>&1)" || {
    printf '%s\n' "$findings" >&2
    exit 2
  }
  ;;
*) ;;
esac
exit 0
