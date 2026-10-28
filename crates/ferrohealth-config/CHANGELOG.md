<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-config

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-config-vX.Y.Z`.

## [Unreleased]

### Added

- `Loaded::table_with_secrets` returns the table the file, the environment
  and the `--set` pairs set, with every renamed key under its new name and
  every override typed as the tree took it. The defaults are left out, so
  the table read again as the file gives the same tree. It holds secrets as
  read, and the `Debug` rendering of `Loaded` leaves it out, so a product
  that keeps configuration revisions takes the table from the loader in
  place of placing the overrides a second time (#156).

## [0.2.6] - 2026-10-27

### Added

- `Loader::defaults` layers a product's defaults for a dotted key, from any
  `Serialize` value such as a shared crate's section, under the file, the
  environment and the `--set` pairs. A key no source sets takes the
  product's value in place of the section type's `Default`, so a partial
  table keeps every product default it does not name, and the provenance
  reports the key as `default` (#140).

## [0.2.5] - 2026-10-26

### Fixed

- The loader no longer refuses a variable under the prefix outside the
  override form. Product prefixes nest (`FERROTERM`, `FERROTERM_CONSOLE`,
  `FERROTERM_SYNC`), so a shell that set one binary's variables stopped
  another from starting. Only an unknown key named by a `<PREFIX>__…`
  override is refused; `strict::sweep` stays for a binary whose prefix no
  other extends (#114).

### Added

- `Loader::reserved` returns the names `Loader::reserve` recorded.

## [0.2.4] - 2026-10-26

### Changed

- Requires `ferrohealth-secret` as `0.2.0`, a caret requirement in place of the exact pin, so a product takes a patch of that crate without waiting for a release of this one.

## [0.2.3] - 2026-10-26

### Fixed

- A refusal of an unknown section that a `<PREFIX>__SECTION__KEY` override
  created names that variable, as a refusal of an unknown key already did
  (#104).

## [0.2.2] - 2026-10-26

### Added

- `loader::Loader::fallback_composed`, a fallback whose value is composed
  from several environment variables, ordered with the other fallbacks and
  below the canonical override (#88).
- `libpq::url`, the `postgres://` URL composed from PostgreSQL's client
  environment (`PGHOST_UNPOOLED` or `PGHOST`, `PGPORT`, `PGDATABASE`,
  `PGUSER`, `PGPASSWORD`, `PGSSLMODE`), its userinfo percent-encoded (#88).

## [0.2.1] - 2026-10-26

### Fixed

- An invalid value in a nested table is refused once: the loader no longer
  adds a `missing field` refusal for the table it dropped the value from to
  look for further refusals (#84).

## [0.2.0] - 2026-10-26

### Added

- A top-level key through the environment: `<PREFIX>__KEY` (#58).
- `Loader::load_with` and `Loader::load_sources_with`, the `--set key=value`
  layer above the environment, reported as the `set` source (#59).
- Comma-separated environment values for a key that wants a sequence (#60).
- `Loader::fallback`, a variable such as `DATABASE_URL` or `PORT` that sits
  below the canonical override and may transform its value (#61).
- `Loader::rename`, a renamed key read under its old name until a release,
  reported in `Loaded::deprecations`, in the log and in `check_report`; both
  names set at once is refused as `ConfigError::Renamed` (#62).
- Up to `MAX_REFUSALS` invalid keys reported at once as
  `ConfigError::Refusals` (#63).

### Changed

- It builds on `ferrohealth-secret` 0.2.0.
- `Loader` no longer implements `PartialEq` and `Eq`, since an alias may
  carry a transform function; `provenance::Source` gains `Set` (#59, #61).

## [0.1.0] - 2026-10-26

### Added

- `loader::Loader`, the one configuration loader of the family: the tree's
  defaults, an optional TOML file named by the caller or by
  `<PREFIX>_CONFIG`, and the environment, where `<PREFIX>__SECTION__KEY`
  sets a key and an alias such as `RUST_LOG` sets the key it names; an
  override is placed typed only where the tree wants a value that is not a
  string (#14).
- Strict loading: an unknown key and a value of the wrong type are refused
  with the dotted key, the variable that set it and the closest known key,
  and every variable under the prefix that is neither an override, an alias
  nor reserved is refused with the spelling it probably meant; no refusal
  quotes a value (#14).
- `provenance::Provenance`, where each key came from: the default, the file
  or the environment with its variable (#14).
- `report::check_report`, the `config check` rendering: every key with its
  value and source, secrets shown as `***` through the types of
  `ferrohealth-secret` (#14).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.6...HEAD
[0.2.6]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.5...ferrohealth-config-v0.2.6
[0.2.5]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.4...ferrohealth-config-v0.2.5
[0.2.4]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.3...ferrohealth-config-v0.2.4
[0.2.3]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.2...ferrohealth-config-v0.2.3
[0.2.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.1...ferrohealth-config-v0.2.2
[0.2.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.2.0...ferrohealth-config-v0.2.1
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-config-v0.1.0...ferrohealth-config-v0.2.0
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-config-v0.1.0
