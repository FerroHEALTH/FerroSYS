#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
#
# The release facts the `plan` job of .github/workflows/release.yml verifies
# at the tagged commit, as ONE ordered list (no specification governs the list
# or its order: our own design). The plan job runs it in CI and
# scripts/release/preflight.sh runs it on a release branch before the tag is
# pushed, so the two cannot check different things.
#
# Every crate under crates/ releases on its own version line and its own
# tag: `<crate>-vX.Y.Z` releases <crate>, with the [package] version of
# crates/<crate>/Cargo.toml and the section of crates/<crate>/CHANGELOG.md.
#
#   plan.sh checks <tag> [--notes <file>]
#       Runs every check below at the working tree, in order, and writes the
#       release notes to <file> (default release-notes.md). A failing check is
#       named and the list goes on, so one run reports every finding; checks
#       that read the notes are skipped when the notes could not be composed.
#       Exit 1 when any check failed.
#   plan.sh prev-tag <tag>
#       Prints the release tag of the same crate before <tag> by version
#       order, read from `git ls-remote` of origin: a pushed tag counts
#       whether or not its release published, and ancestry plays no part.
#   plan.sh crate <tag>
#       Prints the crate <tag> releases.
#   plan.sh changelog <tag>
#       Prints the changelog file of the crate <tag> releases.
#   plan.sh notes <tag> <file>
#       Writes the release notes for <tag>: its changelog section and the
#       support period.
#   plan.sh support-end <YYYY-MM-DD>
#       Prints the month the support period of a release of that date ends.
#   plan.sh --self-test
#       Proves prev-tag's ordering, support-end's arithmetic and the reading
#       of a README install line offline.
#
# The milestone check reads the tracker through gh (GH_TOKEN in CI, the
# logged-in gh locally). Exit 2 on a usage error.
set -euo pipefail

readonly TAG_RE='^ferrohealth-(secret|config|telemetry|health|server|usage-report)-v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'
# The support period in years from the release date: the minimum of
# Regulation (EU) 2024/2847 Art 13(8), as SECURITY.md states it.
readonly SUPPORT_YEARS=5
readonly SECURITY_URL='https://github.com/FerroHEALTH/FerroSYS/blob/main/SECURITY.md#supported-versions'

usage() {
  echo "usage: plan.sh checks <tag> [--notes <file>] | prev-tag <tag> | crate <tag> | changelog <tag> | notes <tag> <file> | support-end <YYYY-MM-DD> | --self-test" >&2
  exit 2
}

valid_tag() {
  local tag="$1"
  [[ "$tag" =~ $TAG_RE ]] || {
    echo "::error::plan: '$tag' is not a ferrohealth-<crate>-vX.Y.Z tag of a FerroSYS crate" >&2
    exit 2
  }
}

# The crate a tag releases, the prefix of its tags, and its changelog.
# The crate is the text before the last `-v`, which no crate name contains.
crate_of() {
  echo "${1%-v*}"
}

tag_prefix() {
  echo "$(crate_of "$1")-"
}

changelog_of() {
  echo "crates/$(crate_of "$1")/CHANGELOG.md"
}

# The version a tag names: the text after its `v`.
version_of() {
  local tag="$1"
  echo "${tag#"$(tag_prefix "$tag")"v}"
}

# The previous release.

# order_prev TAG: reads tag names on stdin and prints the release tag of the
# same crate directly below TAG's release (its pre-release suffix dropped) by
# version order.
order_prev() {
  local prefix version want
  prefix="$(tag_prefix "$1")"
  version="$(version_of "$1")"
  want="${version%%-*}"
  awk -v prefix="$prefix" '
    {
      tag = $0; v = ""
      if (index(tag, prefix "v") == 1) v = substr(tag, length(prefix) + 2)
      if (v ~ /^[0-9]+\.[0-9]+\.[0-9]+$/) print v "\t" tag
    }' | awk -F'\t' -v want="$want" '$1 != want' |
    {
      cat
      printf '%s\t%s\n' "$want" "@"
    } | sort -V -k1,1 | awk -F'\t' '$2 == "@" { print p; exit } { p = $2 }'
}

prev_tag() {
  local tag="$1" prev
  prev="$(git ls-remote --tags --refs origin | sed 's#.*refs/tags/##' | order_prev "$tag")"
  [[ -n "$prev" ]] || {
    echo "::error::plan: no release tag below $tag" >&2
    return 1
  }
  printf '%s\n' "$prev"
}

# The support period.

# support_end YYYY-MM-DD: the month and year SUPPORT_YEARS after the date, as
# GNU `date -d "<date> +5 years" '+%B %Y'` gives it (a 29 February rolls into
# March), computed here so the preflight runs on BSD date too.
support_end() {
  local d="$1" y m day ey em
  local -a months=(January February March April May June July August September October November December)
  local -a mdays=(31 28 31 30 31 30 31 31 30 31 30 31)
  [[ "$d" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})$ ]] || return 1
  y=$((10#${BASH_REMATCH[1]}))
  m=$((10#${BASH_REMATCH[2]}))
  day=$((10#${BASH_REMATCH[3]}))
  ((m >= 1 && m <= 12 && day >= 1)) || return 1
  leap() {
    local year="$1"
    ((year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)))
  }
  if ((m == 2)) && leap "$y"; then ((day <= 29)) || return 1; else ((day <= mdays[m - 1])) || return 1; fi
  ey=$((y + SUPPORT_YEARS))
  em=$m
  if ((m == 2 && day == 29)) && ! leap "$ey"; then em=3; fi
  printf '%s %s\n' "${months[em - 1]}" "$ey"
}

# The release notes.

notes() {
  local out="$2" version changelog released end
  version="$(version_of "$1")"
  changelog="$(changelog_of "$1")"
  if [[ ! -f "$changelog" ]]; then
    echo "::error::$changelog is missing, so $version has no release notes." >&2
    return 1
  fi
  if ! grep -q "^## \[$version\]" "$changelog"; then
    echo "::error::$changelog has no '## [$version]' section. Cut the release section before tagging." >&2
    return 1
  fi
  # Everything under the version heading, stopping at the next version
  # heading or at the trailing link-reference block, whichever comes first.
  awk -v ver="$version" '
    $0 ~ "^## \\[" ver "\\]" { found = 1; next }
    found && /^## \[/ { exit }
    found && /^\[[^]]+\]:/ { exit }
    found { print }
  ' "$changelog" >"$out"
  if ! grep -q '[^[:space:]]' "$out"; then
    echo "::error::the [$version] changelog section is empty." >&2
    return 1
  fi
  # Regulation (EU) 2024/2847 Art 13(19): the end of the support commitment,
  # at least month and year, counted from the release date of the heading.
  released="$(sed -n "s/^## \[$version\] - \([0-9-]*\).*/\1/p" "$changelog" | head -n1)"
  if ! end="$(support_end "$released")"; then
    echo "::error::the [$version] heading carries no release date to count the support period from." >&2
    return 1
  fi
  {
    echo
    echo "### Support period"
    echo
    echo "Only the newest release is supported: there are no maintenance branches, no long-term-support line and no backports, and upgrading is free. Under the Cyber Resilience Act, Cadasto B.V. commits to keep publishing security fixes for at least five years after each release (Art 13(8)) and ships them only in the newest release (Art 13(10)). For this release that commitment runs until the end of $end (Regulation (EU) 2024/2847 Art 13(19)); see [SECURITY.md]($SECURITY_URL)."
  } >>"$out"
}

# The checks, one function each, run in the order of `list`.

# A release is cut only when its milestone is empty.
check_milestone() { bash scripts/release/milestone.sh check "$TAG"; }

# The tag names the version of the crate it releases: the [package] version
# of its own manifest.
check_version() {
  local manifest table="[package]" file="crates/$CRATE/Cargo.toml"
  if [[ ! -f "$file" ]]; then
    echo "::error::tag $TAG names $CRATE, which has no $file." >&2
    return 1
  fi
  manifest="$(awk -v table="$table" '
    /^[[:space:]]*\[/ { h = $0; gsub(/[[:space:]]/, "", h); f = (h == table); next }
    f && $0 ~ /^[[:space:]]*version[[:space:]]*=/ {
      if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
    }
  ' "$file")"
  if [[ "$manifest" != "$VERSION" ]]; then
    echo "::error::tag $TAG declares version $VERSION, but $file says ${manifest:-nothing}." >&2
    return 1
  fi
  echo "OK: $file declares $VERSION"
}

check_changelog_structure() { bash scripts/checks/changelog-structure.sh "$CHANGELOG"; }

# The cut refreshes the link references: [Unreleased] compares from the tag
# being cut and the cut version has a link of its own (Keep a Changelog 1.1.0
# writes each version heading as a link). A pre-release cuts no section.
check_changelog_links() {
  local base="https://github.com/FerroHEALTH/FerroSYS" failed=0
  if [[ "$VERSION" == *-* ]]; then
    echo "a pre-release cuts no changelog section"
    return 0
  fi
  if ! grep -qxF "[Unreleased]: $base/compare/$TAG...HEAD" "$CHANGELOG"; then
    echo "::error::$CHANGELOG: the [Unreleased] link does not compare from $TAG; write '[Unreleased]: $base/compare/$TAG...HEAD'." >&2
    failed=1
  fi
  if ! grep -q "^\[$VERSION\]: $base/" "$CHANGELOG"; then
    echo "::error::$CHANGELOG: the [$VERSION] heading has no link reference; add '[$VERSION]: $base/compare/<previous tag>...$TAG'." >&2
    failed=1
  fi
  [[ "$failed" -eq 0 ]] && echo "the [Unreleased] and [$VERSION] links name $TAG"
}

# Regulation (EU) 2024/2847 Annex I Part II(2), (4) and (8): the section
# being cut says why a security fix ships with functional changes and names
# every advisory.
check_changelog_security() {
  bash scripts/checks/changelog-security.sh --self-test
  VEX_ROOT=. bash scripts/checks/changelog-security.sh --section "$VERSION" "$CHANGELOG"
}

# readme_pins CRATE: reads a README on stdin and prints the version of every
# install line that pins CRATE exactly (`<crate> = "=X.Y.Z"` or
# `<crate> = { version = "=X.Y.Z", ... }`), one per line.
readme_pins() {
  awk -v crate="$1" '
    index($0, crate " = ") == 1 && match($0, /"=[^"]*"/) { print substr($0, RSTART + 2, RLENGTH - 3) }
  '
}

# The crate README is its crates.io page: its install line pins the version
# being released. A pre-release keeps the line of the release before it.
check_readme_pin() {
  local readme="crates/$CRATE/README.md" pins stale
  if [[ "$VERSION" == *-* ]]; then
    echo "a pre-release moves no install line"
    return 0
  fi
  if [[ ! -f "$readme" ]]; then
    echo "::error::$CRATE has no $readme." >&2
    return 1
  fi
  pins="$(readme_pins "$CRATE" <"$readme")"
  if [[ -z "$pins" ]]; then
    echo "::error::$readme has no install line pinning $CRATE as '$CRATE = \"=$VERSION\"'." >&2
    return 1
  fi
  stale="$(grep -vxF "$VERSION" <<<"$pins" || true)"
  if [[ -n "$stale" ]]; then
    echo "::error::$readme pins $CRATE at $(paste -sd, - <<<"$stale"); the release is $VERSION." >&2
    return 1
  fi
  echo "OK: $readme pins $VERSION"
}

check_notes() {
  notes "$TAG" "$NOTES"
  echo "Release notes for $VERSION ($(wc -l <"$NOTES" | tr -d ' ') lines):"
  cat "$NOTES"
}

check_notes_content() {
  if ! grep -q 'commitment runs until the end of [A-Z][a-z]* [0-9]\{4\} ' "$NOTES"; then
    echo "::error::the release notes for $TAG do not state the support period (Regulation (EU) 2024/2847 Art 13(19))." >&2
    return 1
  fi
  echo "the notes carry the support period"
}

# The list: "<function> <title>", in the order the plan job runs them. A
# check that reads the notes comes after check_notes.
list() {
  cat <<'EOF'
check_milestone The milestone titled as the tag holds no open item
check_version The tag matches the version of the crate it releases
check_changelog_structure Changelog structure (no duplicate subsections, Keep a Changelog types only)
check_changelog_links The changelog links compare from the tag being cut
check_changelog_security The Security section of the cut names its advisories and its reason
check_readme_pin The crate README's install line pins the version being released
check_notes The changelog section for this tag becomes the release notes
check_notes_content The release notes carry the support period
EOF
}

checks() {
  TAG="$1"
  VERSION="$(version_of "$TAG")"
  CRATE="$(crate_of "$TAG")"
  CHANGELOG="$(changelog_of "$TAG")"
  NOTES="$2"
  local fn title notes_ok=1 rc
  local -a failed=()
  while read -r fn title; do
    if [[ "$fn" == check_notes_content && "$notes_ok" -eq 0 ]]; then
      echo "skipped: $title (no notes were composed)"
      failed+=("$title (skipped: no notes)")
      continue
    fi
    if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then echo "::group::$title"; else echo "-- $title"; fi
    set +e
    (
      set -e
      "$fn"
    )
    rc=$?
    set -e
    [[ "${GITHUB_ACTIONS:-}" != "true" ]] || echo "::endgroup::"
    if [[ "$rc" -ne 0 ]]; then
      echo "::error::plan: refused: $title" >&2
      failed+=("$title")
      [[ "$fn" != check_notes ]] || notes_ok=0
    fi
  done < <(list)
  if [[ "${#failed[@]}" -gt 0 ]]; then
    echo "plan: ${#failed[@]} check(s) refused $TAG:" >&2
    printf '  %s\n' ${failed[@]+"${failed[@]}"} >&2
    return 1
  fi
  echo "plan: every release fact for $TAG verified at this tree"
}

self_test() {
  local fails=0 got refused="refused"
  expect() {
    local name="$1" have="$2" want="$3"
    if [[ "$have" != "$want" ]]; then
      echo "self-test: $name: got '$have', want '$want'" >&2
      fails=$((fails + 1))
    fi
  }
  local secret=ferrohealth-secret-v
  got="$(printf '%s\n' ${secret}0.0.9 ${secret}0.1.0 ${secret}0.0.10 ${secret}0.0.8 ${secret}0.0.11-rc.1 ${secret}0.0.11 | order_prev ${secret}0.0.11)"
  expect "version order, not lexical" "$got" "${secret}0.0.10"
  got="$(printf '%s\n' ${secret}0.0.9 ${secret}0.0.10 ${secret}0.0.11 | order_prev ${secret}0.0.10)"
  expect "the tag itself is skipped" "$got" "${secret}0.0.9"
  got="$(printf '%s\n' ${secret}0.0.10 ${secret}0.0.11 | order_prev ${secret}0.0.11-rc.1)"
  expect "a pre-release compares as its release" "$got" "${secret}0.0.10"
  got="$(printf '%s\n' ${secret}0.0.10 ${secret}0.0.11 | order_prev ${secret}0.0.12)"
  expect "an unpushed tag" "$got" "${secret}0.0.11"
  got="$(printf '%s\n' ${secret}0.9.0 ${secret}1.10.0 ${secret}1.9.0 | order_prev ${secret}1.10.0)"
  expect "a minor above nine" "$got" "${secret}1.9.0"
  got="$(printf '%s\n' ${secret}0.0.11 | order_prev ${secret}0.0.1)"
  expect "no tag below" "$got" ""
  got="$(printf '%s\n' ${secret}0.1.0 ferrohealth-config-v0.1.10 ferrohealth-config-v0.1.9 | order_prev ferrohealth-config-v0.1.10)"
  expect "a crate's tags order apart from another crate's" "$got" "ferrohealth-config-v0.1.9"
  got="$(printf '%s\n' ferrohealth-usage-report-v0.1.0 ${secret}0.2.0 | order_prev ferrohealth-usage-report-v0.2.0)"
  expect "a hyphenated crate name" "$got" "ferrohealth-usage-report-v0.1.0"
  expect "the crate of a secret tag" "$(crate_of ${secret}0.1.0)" "ferrohealth-secret"
  expect "the crate of a usage-report pre-release" "$(crate_of ferrohealth-usage-report-v0.1.0-rc.1)" "ferrohealth-usage-report"
  expect "the version of a pre-release" "$(version_of ferrohealth-server-v0.1.0-rc.1)" "0.1.0-rc.1"
  expect "the changelog of a tag" "$(changelog_of ferrohealth-health-v0.1.0)" "crates/ferrohealth-health/CHANGELOG.md"
  expect "a plain date" "$(support_end 2026-10-23)" "October 2031"
  expect "a leap day into a common year" "$(support_end 2028-02-29)" "March 2033"
  expect "a leap day in a common year" "$(support_end 2027-02-29 || echo "$refused")" "$refused"
  expect "the last day of a month" "$(support_end 2026-12-31)" "December 2031"
  expect "an empty date" "$(support_end "" || echo "$refused")" "$refused"
  expect "a month out of range" "$(support_end 2026-13-01 || echo "$refused")" "$refused"
  expect "a day out of range" "$(support_end 2026-04-31 || echo "$refused")" "$refused"
  got="$(printf '%s\n' '```toml' '[dependencies]' 'ferrohealth-secret = "=0.2.0"' '```' | readme_pins ferrohealth-secret)"
  expect "a plain install line" "$got" "0.2.0"
  got="$(printf '%s\n' 'ferrohealth-telemetry = { version = "=0.1.0", features = ["axum"] }' | readme_pins ferrohealth-telemetry)"
  expect "an install line with features" "$got" "0.1.0"
  got="$(printf '%s\n' 'ferrohealth-secret = "=0.2.0"' 'ferrohealth-config = "=0.1.0"' | readme_pins ferrohealth-config)"
  expect "another crate's line is not this crate's" "$got" "0.1.0"
  got="$(printf '%s\n' 'Pin ferrohealth-health = "=9.9.9" in prose.' 'ferrohealth-health = "0.1"' | readme_pins ferrohealth-health)"
  expect "prose and an inexact requirement are no install line" "$got" ""
  if [[ "$fails" -gt 0 ]]; then
    echo "plan.sh self-test FAILED ($fails case(s))" >&2
    exit 1
  fi
  echo "plan.sh self-test OK"
}

main() {
  case "${1:-}" in
  checks)
    local tag="${2:-}" flag="${3:-}" notes_file="${4:-}"
    [[ "$#" -eq 2 || ("$#" -eq 4 && "$flag" == "--notes" && -n "$notes_file") ]] || usage
    valid_tag "$tag"
    notes_file="${notes_file:-release-notes.md}"
    [[ "$notes_file" == /* ]] || notes_file="$PWD/$notes_file"
    cd "$(dirname "$0")/../.."
    checks "$tag" "$notes_file"
    ;;
  prev-tag)
    local tag="${2:-}"
    [[ "$#" -eq 2 ]] || usage
    valid_tag "$tag"
    prev_tag "$tag"
    ;;
  crate)
    [[ "$#" -eq 2 ]] || usage
    valid_tag "$2"
    crate_of "$2"
    ;;
  changelog)
    [[ "$#" -eq 2 ]] || usage
    valid_tag "$2"
    changelog_of "$2"
    ;;
  notes)
    local tag="${2:-}" out="${3:-}"
    [[ "$#" -eq 3 ]] || usage
    valid_tag "$tag"
    [[ "$out" == /* ]] || out="$PWD/$out"
    cd "$(dirname "$0")/../.."
    notes "$tag" "$out"
    ;;
  support-end)
    local date="${2:-}"
    [[ "$#" -eq 2 ]] || usage
    support_end "$date" || {
      echo "::error::plan: '$date' is not a YYYY-MM-DD date" >&2
      exit 1
    }
    ;;
  --self-test)
    [[ "$#" -eq 1 ]] || usage
    self_test
    ;;
  *) usage ;;
  esac
}

main "$@"
