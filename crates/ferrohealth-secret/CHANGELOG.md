<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-secret

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-secret-vX.Y.Z`.

## [Unreleased]

### Added

- `dir::SecretDir`, named secrets in one directory, one file per secret,
  read again when the directory changes, for a secret a product learns the
  name of while it runs (#425). `open` reads the directory into a
  `dir::Snapshot`, `refresh` reads it again, and `watch` refreshes on a
  thread at a fixed interval and hands a failed read to a callback, keeping
  the last snapshot. A Kubernetes Secret mounted without `subPath` is read
  from the directory its `..data` symlink names, so a read sees one update
  whole. `get` resolves a name to a `SecretString`, `get_pair` reads a
  client certificate from `<name>.crt` and `<name>.key`, and `names` lists
  what exists. A name is one path segment with no leading dot, and every
  `dir::SecretDirError` names the secret or the directory and never a value.
  `dir::SecretDirConfig` is the `dir` and `poll_seconds` configuration, with
  `deny_unknown_fields`.

## [0.2.1] - 2026-11-10

### Added

- Feature `schema`: `schemars::JsonSchema` for `secret::Secret` (a
  `writeOnly` string) and `secret_url::SecretUrl` (a string whose userinfo
  and query render masked), each describing the inline form and the
  `<key>_file` sibling and carrying no value, for a product that generates
  its configuration reference from its types (#360).

## [0.2.0] - 2026-10-26

### Added

- `secret::WriteOnly`, a secret on its one trip from a console to its
  server: redacted in `Debug` and `Display`, its value in `Serialize`; and
  `Default` for `Secret`, the empty secret (#64).

## [0.1.0] - 2026-10-26

### Added

- `secret::Secret`, a credential held in a `secrecy::SecretString` (zeroed
  on drop) that renders as `***` through `Debug`, `Display` and `Serialize`
  and deserializes from a plain string (#13).
- `secret_url::SecretUrl`, a URL or connection string that renders with its
  userinfo and query masked, and `secret_url::RedactedUrl`, the same
  rendering for any URL text: the one URL mask of the family (#13).
- `redacted::Redacted<T>`, the `Debug` and `Display` for a value holding
  clinical content or a patient identifier: the type's path and `***` (#13).
- `file::read_secret`, `file::require_secret` and `file::read_secret_file`,
  the rule that reads a secret from its key or from the file its
  `<key>_file` sibling names, exactly one of the two, with the trailing line
  ending removed, an empty file refused, and every error naming the key and
  never the secret (#13).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-secret-v0.2.1...HEAD
[0.2.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-secret-v0.2.0...ferrohealth-secret-v0.2.1
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-secret-v0.1.0...ferrohealth-secret-v0.2.0
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-secret-v0.1.0
