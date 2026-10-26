<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-config

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-config-vX.Y.Z`.

## [Unreleased]

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
