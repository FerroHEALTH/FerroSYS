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
# A public dependency (scripts/lib/public-deps.sh) required on another
# compatibility line than the base requires it on breaks the crate where
# cargo-semver-checks cannot see it (#349), so the crate takes the next minor
# at patch 0 in the same change (.claude/rules/crates-publishing.md). A base
# version with no `<crate>-v<version>` tag on origin is a minor an earlier
# change opened and no release carries yet, so the move rides on it.
#
# Usage: scripts/checks/crate-version-guard.sh <base-ref> [head-ref]
#        scripts/checks/crate-version-guard.sh --self-test
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck disable=SC1091 # shellcheck is not run with -x; the library is checked on its own
. scripts/lib/public-deps.sh

# Each case builds a one-crate workspace in a scratch repository whose origin
# is itself, and runs a copy of this script there.
self_test() {
  local guard tmp failures=0
  guard="$PWD/scripts/checks/crate-version-guard.sh"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
  t_git() { git -C "$tmp/repo" -c user.name=self-test -c user.email=self-test@example.invalid -c commit.gpgsign=false -c tag.gpgsign=false "$@"; }
  # The crate at version $1, requiring ferrohealth-demo on $2 and serde on $3.
  state() {
    printf '[workspace]\nmembers = ["crates/demo"]\n\n[workspace.dependencies]\nferrohealth-demo = "%s"\nserde = "%s"\n' "$2" "$3" > "$tmp/repo/Cargo.toml"
    printf '[package]\nname = "demo"\nversion = "%s"\n\n[dependencies]\nferrohealth-demo.workspace = true\nserde = { workspace = true, optional = true }\n' "$1" > "$tmp/repo/crates/demo/Cargo.toml"
    printf '[[package]]\nname = "demo"\nversion = "%s"\n' "$1" > "$tmp/repo/Cargo.lock"
    grep -qF "## [$1]" "$tmp/repo/crates/demo/CHANGELOG.md" 2> /dev/null ||
      printf '## [%s]\n' "$1" >> "$tmp/repo/crates/demo/CHANGELOG.md"
    t_git add -A
    t_git commit -qm "demo $1 on ferrohealth-demo $2, serde $3"
  }
  expect() {
    local want="$1" label="$2" got=0
    shift 2
    (cd "$tmp/repo" && bash scripts/checks/crate-version-guard.sh "$@" > /dev/null 2>&1) || got=1
    if [[ "$got" -ne "$want" ]]; then
      echo "crate-version-guard: self-test: $label: exit $got, wanted $want" >&2
      failures=$((failures + 1))
    fi
  }

  mkdir -p "$tmp/repo/crates/demo" "$tmp/repo/scripts/checks" "$tmp/repo/scripts/lib"
  git init -q -b main "$tmp/repo"
  cp "$guard" "$tmp/repo/scripts/checks/"
  cp "$PWD/scripts/lib/public-deps.sh" "$tmp/repo/scripts/lib/"
  t_git remote add origin "$tmp/repo"
  state 0.1.3 0.4.0 1.0.0
  t_git tag demo-v0.1.3
  t_git branch released

  state 0.1.4 0.4.2 1.0.0
  expect 0 "a patch step on the public dependency's line" released HEAD
  state 0.1.2 0.4.2 1.0.0
  expect 1 "a version that moves backwards" released HEAD
  state 0.1.3 0.5.0 1.0.0
  expect 1 "a public dependency on another line at the released version" released HEAD
  state 0.1.4 0.5.0 1.0.0
  expect 1 "a public dependency on another line with a patch step" released HEAD
  state 0.2.1 0.5.0 1.0.0
  expect 1 "a public dependency on another line with a minor step off patch 0" released HEAD
  state 0.2.0 0.5.0 1.0.0
  expect 0 "a public dependency on another line with the next minor" released HEAD
  t_git branch opened
  state 0.2.0 0.6.0 1.0.0
  expect 0 "a second move on a minor no release carries yet" opened HEAD
  t_git checkout -q -b private released
  state 0.1.4 0.4.0 2.0.0
  expect 0 "a private dependency on another line with a patch step" released HEAD

  if [[ "$failures" -ne 0 ]]; then
    echo "crate-version-guard: self-test: $failures case(s) failed" >&2
    return 1
  fi
  echo "crate-version-guard: self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit
fi

base="${1:?usage: crate-version-guard.sh <base-ref> [head-ref] | --self-test}"
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
  local member="$1"
  awk -v name="$member" '$0 == "name = \"" name "\"" {p=1; next}
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
  if [[ -n "$was" && "$was" != "0.0.0" ]]; then
    moved="$(moved_lines \
      "$(manifest_requirements <(git show "$base:Cargo.toml") <(git show "$base:$manifest") | public_lines)" \
      "$(manifest_requirements <(git show "$head:Cargo.toml") <(git show "$head:$manifest") | public_lines)")"
    if [[ -n "$moved" ]]; then
      if [[ "$(compat_line "$was")" == "$(compat_line "$want")" ]]; then
        released=0
        git ls-remote --exit-code --tags --refs origin "refs/tags/$crate-v$was" > /dev/null || released=$?
        case "$released" in
          0) fail "$crate stays on the line of its release $was at $want while a public dependency moves to another compatibility line ($moved); that breaks its consumers, so it takes the next minor at patch 0" ;;
          2) ;;
          *) fail "cannot read the tags of origin to tell whether $crate $was is released" ;;
        esac
      elif [[ "${want##*.}" != "0" ]]; then
        fail "$crate leaves its line at $want while a public dependency moves ($moved); the next minor starts at patch 0"
      fi
    fi
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
