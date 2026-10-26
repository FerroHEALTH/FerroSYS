<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-secret

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-secret-vX.Y.Z`.

## [Unreleased]

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
