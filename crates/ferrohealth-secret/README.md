<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-secret

The secret and redaction types of the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). Redaction is a property
of the type, so a secret or a patient value cannot reach a log line, a
`Debug` rendering, an error message or a rendered configuration by accident.

## What it does

- `secret::Secret` holds a credential and renders as `***` through `Debug`,
  `Display` and `Serialize`. The value is a `secrecy::SecretString`, zeroed
  on drop, and is reached only through `expose`.
- `secret_url::SecretUrl` holds a URL or connection string and renders it
  with its userinfo and query masked (`postgres://***@db.example.org/x`).
  `secret_url::RedactedUrl` gives any URL text the same rendering.
- `redacted::Redacted<T>` gives a value holding clinical content or a
  patient identifier a `Debug` that names its type and shows none of it.
- `file::read_secret` and `file::require_secret` read a secret from its key
  or from the file its `<key>_file` sibling names, exactly one of the two,
  with the trailing line ending removed and an empty file refused.

## Use

```toml
[dependencies]
ferrohealth-secret = "=0.1.0"
```

```rust
use ferrohealth_secret::file::require_secret;
use ferrohealth_secret::secret::Secret;

fn password(inline: Option<Secret>, file: Option<&std::path::Path>) -> Result<Secret, ferrohealth_secret::file::SecretError> {
    require_secret("cdr.password", inline, file)
}
```

Pin the version exactly, as every FerroHEALTH product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
