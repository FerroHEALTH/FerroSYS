#!/usr/bin/env bash
# SPDX-FileCopyrightText: Cadasto B.V.
# SPDX-License-Identifier: BUSL-1.1
# Keeps each crate's declaration of its third-party public dependencies true
# to its public API (#356). The declaration is the `public-dependencies` array
# of the `[package.metadata.ferrohealth]` table of the crate's manifest, which
# scripts/lib/public-deps.sh reads for crate-version-guard.sh and semver.sh:
# a line move of a declared crate takes the next minor (The Cargo Book,
# "SemVer Compatibility", introduction).
#
# For each crate with a library, rustdoc writes its API as JSON with every
# feature on, the superset of what any product compiles against. A crate is
# on the surface when an item of that API (a signature, a field, a bound, a
# trait implementation, a re-export) names an item it defines. Left out are
# the standard library, the blanket and auto-trait implementations rustdoc
# adds by itself, and paths through a `__`-prefixed module, the hidden
# support of a macro (pin-project-lite's `__private`) that no consumer
# names. No specification says which dependencies a crate exposes: these
# exclusions are our own design.
#
# A surface crate that is a direct dependency is attributed to itself. One
# that is not (axum_core, serde_core, rustls_pki_types) is reached through a
# direct dependency that re-exports it (`pub use` or `pub extern crate` in its
# source), and each such re-exporter is attributed. A family or sibling crate
# counts by rule and is never declared. The check fails when the declaration
# misses an attributed crate or names one that is not attributed.
#
# rustdoc's JSON output is unstable, so the build sets RUSTC_BOOTSTRAP to the
# documented crate alone, the way cargo-semver-checks builds its rustdoc.
#
# Usage: scripts/checks/public-surface.sh [<crate>...]
#        scripts/checks/public-surface.sh --self-test
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck disable=SC1091 # shellcheck is not run with -x; the library is checked on its own
. scripts/lib/public-deps.sh
# shellcheck disable=SC1091 # shellcheck is not run with -x; the library is checked on its own
. scripts/lib/guard-args.sh

grammar="[<crate>...] | --self-test"
guard_known_flags "$grammar" "--self-test" "$@"

# The library names of the crates the API in the rustdoc JSON $1 names items
# of, one per line.
surface() {
  jq -r '
    . as $d
    | [ .index[]
        | select(.crate_id == 0)
        | select((.inner.impl // {}) | (.blanket_impl == null and (.is_synthetic | not)))
        | .inner | .. | objects | select(has("id")) | .id | select(. != null)
        | $d.paths[tostring] // empty
        | select(.crate_id != 0 and (.path | any(startswith("__")) | not))
        | $d.external_crates[.crate_id | tostring].name ]
    | unique
    | map(select(IN("core", "alloc", "std", "proc_macro", "test") | not))
    | .[]' "$1"
}

# How package $2 reaches each library name on stdin, from the `cargo metadata`
# document $1: `direct <lib> <package>` for a direct normal dependency,
# `via <lib> <package> <alias> <source dir>` for each direct normal dependency
# that depends on it under `alias`, and `none <lib>` when neither holds.
attribution() {
  jq -r --arg pkg "$2" --argjson surface "$(jq -R . | jq -s .)" '
    def lib: [.targets[] | select(.kind | any(. == "lib" or . == "rlib" or . == "dylib" or . == "proc-macro"))] | first;
    (.packages | map(lib as $l | {key: .id, value: {name, lib: ($l.name // "" | gsub("-"; "_")), src: ($l.src_path // "" | sub("/[^/]*$"; ""))}}) | from_entries) as $p
    | (.resolve.nodes | map({key: .id, value: [.deps[] | select(.dep_kinds | any(.kind == null)) | {pkg, name}]}) | from_entries) as $n
    | ([.packages[] | select(.name == $pkg and .source == null) | .id] | first) as $me
    | $n[$me] as $direct
    | $surface[] as $x
    | [$direct[] | select($p[.pkg].lib == $x)] as $hit
    | if ($hit | length) > 0 then "direct \($x) \($p[$hit[0].pkg].name)"
      else
        [$direct[] | . as $d | ($n[$d.pkg] // [])[] | select($p[.pkg].lib == $x)
          | "via \($x) \($p[$d.pkg].name) \(.name) \($p[$d.pkg].src)"] as $via
        | if ($via | length) > 0 then $via[] else "none \($x)" end
      end' "$1"
}

# The packages the attribution lines on stdin name as public dependencies,
# family and sibling crates left out, one per line; the surface crates no
# direct dependency re-exports go to stderr as `unattributed <lib>`.
required() {
  local kind lib package alias dir reached="" attributed=""
  while read -r kind lib package alias dir; do
    case "$kind" in
      direct)
        public_dependency "$package" "" || echo "$package"
        attributed="$attributed$lib"$'\n'
        ;;
      via)
        reached="$reached$lib"$'\n'
        if grep -rqE --include='*.rs' \
          "pub[[:space:]]+(use[[:space:]]+(::)?$alias\\b|extern[[:space:]]+crate[[:space:]]+$alias\\b)" "$dir"; then
          public_dependency "$package" "" || echo "$package"
          attributed="$attributed$lib"$'\n'
        fi
        ;;
      none) reached="$reached$lib"$'\n' ;;
    esac
  done
  while read -r lib; do
    [[ -z "$lib" ]] || grep -qxF -- "$lib" <<<"$attributed" || echo "unattributed $lib" >&2
  done < <(printf '%s' "$reached" | LC_ALL=C sort -u)
}

# Compares the declaration of crate $1 (the names $3) with what its API
# attributes (the names $2), printing each difference.
compare() {
  local crate="$1" want="$2" declared="$3" name
  while read -r name; do
    [[ -n "$name" ]] || continue
    grep -qxF -- "$name" <<<"$declared" ||
      echo "$crate: its public API names items of $name, which [package.metadata.ferrohealth] public-dependencies leaves out"
  done <<<"$want"
  while read -r name; do
    [[ -n "$name" ]] || continue
    if public_dependency "$name" ""; then
      echo "$crate: public-dependencies names $name, a family or workspace crate that counts without a declaration"
    elif ! grep -qxF -- "$name" <<<"$want"; then
      echo "$crate: public-dependencies names $name, whose items its public API does not name"
    fi
  done <<<"$declared"
}

# The findings for crate $1 from its rustdoc JSON $2, the `cargo metadata`
# document $3 and its manifest $4. Returns 2 when a document cannot be read.
findings() {
  local crate="$1" json="$2" metadata="$3" manifest="$4" want declared unattributed
  unattributed="$(mktemp)"
  want="$(surface "$json" | attribution "$metadata" "$crate" | required 2> "$unattributed" | LC_ALL=C sort -u)" || {
    rm -f "$unattributed"
    echo "public-surface: cannot read the API of $crate from $json and $metadata" >&2
    return 2
  }
  while read -r _ lib; do
    [[ -n "${lib:-}" ]] || continue
    echo "$crate: its public API names items of $lib, which no direct dependency re-exports; depend on it directly or name it through the crate that re-exports it"
  done < <(LC_ALL=C sort "$unattributed")
  rm -f "$unattributed"
  declared="$(declared_public < "$manifest" | LC_ALL=C sort -u)"
  compare "$crate" "$want" "$declared"
}

self_test() {
  local tmp failures=0
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  mkdir -p "$tmp/axum/src" "$tmp/tower/src" "$tmp/hyper-util/src" "$tmp/rustls/src" "$tmp/demo/src" "$tmp/crates/ferrohealth-x"
  echo 'pub use axum_core::response::IntoResponse;' > "$tmp/axum/src/lib.rs"
  echo 'pub use tower_layer::Layer;' > "$tmp/tower/src/lib.rs"
  echo 'pub(crate) use tower_layer::Layer;' > "$tmp/hyper-util/src/lib.rs"
  echo 'pub use pki_types;' > "$tmp/rustls/src/lib.rs"
  # A package `name` with library `lib` whose source is the directory `dir`.
  package() {
    printf '{"id": "%s", "name": "%s", "source": %s, "targets": [{"name": "%s", "kind": ["lib"], "src_path": "%s/src/lib.rs"}]}' \
      "$1" "$1" "$2" "$3" "$tmp/$1"
  }
  {
    printf '{"packages": ['
    package demo null demo
    printf ','
    package ferrohealth-x null ferrohealth_x
    for name in axum axum-core tower tower-layer hyper-util rustls rustls-pki-types pin-project-lite serde; do
      printf ','
      package "$name" '"registry"' "${name//-/_}"
    done
    printf '], "resolve": {"nodes": [
      {"id": "demo", "deps": [
        {"pkg": "axum", "name": "axum", "dep_kinds": [{"kind": null}]},
        {"pkg": "tower", "name": "tower", "dep_kinds": [{"kind": null}]},
        {"pkg": "hyper-util", "name": "hyper_util", "dep_kinds": [{"kind": null}]},
        {"pkg": "rustls", "name": "rustls", "dep_kinds": [{"kind": null}]},
        {"pkg": "pin-project-lite", "name": "pin_project_lite", "dep_kinds": [{"kind": null}]},
        {"pkg": "ferrohealth-x", "name": "ferrohealth_x", "dep_kinds": [{"kind": null}]},
        {"pkg": "serde", "name": "serde", "dep_kinds": [{"kind": "dev"}]}]},
      {"id": "axum", "deps": [{"pkg": "axum-core", "name": "axum_core", "dep_kinds": [{"kind": null}]}]},
      {"id": "tower", "deps": [{"pkg": "tower-layer", "name": "tower_layer", "dep_kinds": [{"kind": null}]}]},
      {"id": "hyper-util", "deps": [{"pkg": "tower-layer", "name": "tower_layer", "dep_kinds": [{"kind": null}]}]},
      {"id": "rustls", "deps": [{"pkg": "rustls-pki-types", "name": "pki_types", "dep_kinds": [{"kind": null}]}]}
    ]}}'
  } > "$tmp/metadata.json"
  # The rustdoc JSON of an API that names an item of each crate in $@, a
  # crate `name@__private` naming it through a hidden module, and `name^` in a
  # blanket implementation only.
  api() {
    local crates="" paths="" index="" i=0 spec name hidden blanket
    for spec in "$@"; do
      i=$((i + 1))
      name="${spec%%[@^]*}"
      hidden=false blanket=null
      [[ "$spec" == *@__private ]] && hidden=true
      [[ "$spec" == *^ ]] && blanket='{"primitive": "u8"}'
      crates="$crates${crates:+,}\"$i\": {\"name\": \"$name\"}"
      if [[ "$hidden" == true ]]; then
        paths="$paths${paths:+,}\"$((i + 100))\": {\"crate_id\": $i, \"path\": [\"$name\", \"__private\", \"T\"]}"
      else
        paths="$paths${paths:+,}\"$((i + 100))\": {\"crate_id\": $i, \"path\": [\"$name\", \"T\"]}"
      fi
      index="$index${index:+,}\"$((i + 200))\": {\"crate_id\": 0, \"inner\": {\"impl\": {\"is_synthetic\": false, \"blanket_impl\": $blanket, \"for\": {\"resolved_path\": {\"path\": \"T\", \"id\": $((i + 100))}}}}}"
    done
    printf '{"index": {%s}, "paths": {%s}, "external_crates": {%s}}' "$index" "$paths" "$crates" > "$tmp/api.json"
  }
  # The manifest declaring the names in $@.
  declare_public() {
    local list="" name
    for name in "$@"; do list="$list${list:+, }\"$name\""; done
    printf '[package]\nname = "demo"\n\n[package.metadata.ferrohealth]\npublic-dependencies = [%s]\n' "$list" > "$tmp/Cargo.toml"
  }
  expect() {
    local want="$1" label="$2" got
    if ! got="$(cd "$tmp" && findings demo "$tmp/api.json" "$tmp/metadata.json" "$tmp/Cargo.toml")"; then
      echo "public-surface: self-test: $label: the check could not run" >&2
      failures=$((failures + 1))
      return
    fi
    got="$(grep -c . <<<"$got" || true)"
    if [[ "$got" -ne "$want" ]]; then
      echo "public-surface: self-test: $label: $got finding(s), wanted $want" >&2
      failures=$((failures + 1))
    fi
  }

  api axum rustls core std
  declare_public axum rustls
  expect 0 "a declaration that names the direct surface crates"
  declare_public axum
  expect 1 "a rustls on the surface left out"
  declare_public axum rustls tower
  expect 1 "a tower declared that the surface does not name"
  api axum_core rustls_pki_types tower_layer
  declare_public axum rustls tower
  expect 0 "crates reached through the dependencies that re-export them"
  declare_public axum tower
  expect 1 "a rustls_pki_types whose re-exporter is left out"
  declare_public axum rustls tower hyper-util
  expect 1 "a hyper-util that depends on tower_layer without re-exporting it"
  api axum pin_project_lite@__private serde^
  declare_public axum
  expect 0 "a macro's hidden module and a blanket implementation"
  api ferrohealth_x axum
  declare_public axum ferrohealth-x
  expect 1 "a family crate declared"
  declare_public axum
  expect 0 "a family crate on the surface"
  api axum unknown_core
  declare_public axum
  expect 1 "a surface crate no direct dependency reaches"

  if [[ "$failures" -ne 0 ]]; then
    echo "public-surface: self-test: $failures case(s) failed" >&2
    return 1
  fi
  echo "public-surface: self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  [[ "$#" -eq 1 ]] || guard_usage "$grammar"
  self_test
  exit
fi

metadata="$(mktemp)"
trap 'rm -f "$metadata"' EXIT
cargo metadata --format-version 1 --all-features --locked > "$metadata"
target="$(jq -r '.target_directory' "$metadata")"

crates=("$@")
if [[ "${#crates[@]}" -eq 0 ]]; then
  while read -r crate; do
    crates+=("$crate")
  done < <(jq -r '.packages[] | select(.source == null and (.manifest_path | contains("/crates/")))
    | select(any(.targets[]; .kind | any(. == "lib"))) | .name' "$metadata" | LC_ALL=C sort)
fi

failed=0
for crate in "${crates[@]}"; do
  [[ -f "crates/$crate/Cargo.toml" ]] || { echo "public-surface: no crate crates/$crate" >&2; exit 2; }
  lib="${crate//-/_}"
  RUSTC_BOOTSTRAP="$lib" cargo rustdoc --quiet --locked -p "$crate" --lib --all-features \
    -- -Z unstable-options --output-format json
  found="$(findings "$crate" "$target/doc/$lib.json" "$metadata" "crates/$crate/Cargo.toml")" || exit 2
  if [[ -n "$found" ]]; then
    while IFS= read -r line; do
      echo "public-surface: $line" >&2
    done <<<"$found"
    failed=1
  else
    echo "public-surface: $crate declares $(declared_public < "crates/$crate/Cargo.toml" | paste -sd' ' - | sed 's/^$/no third-party crate/')"
  fi
done
if [[ "$failed" -ne 0 ]]; then
  echo "public-surface: declare each third-party crate the public API names, and only those (.claude/rules/crates-publishing.md)" >&2
  exit 1
fi
