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
- `dir::SecretDir` resolves a secret by a name learned at runtime, such as
  a database row that names its HMAC key or client certificate, from a
  directory with one file per secret, and reads the directory again when it
  changes (see below).
- Feature `schema` implements `schemars::JsonSchema` for `Secret` and
  `SecretUrl`, for a product that generates its configuration reference.
  Each schema is a string whose description names the `<key>_file` form and
  what renders masked; none carries a default, an example or a pattern.

## Use

```toml
[dependencies]
ferrohealth-secret = "=0.2.2"
```

```rust
use ferrohealth_secret::file::require_secret;
use ferrohealth_secret::secret::Secret;

fn password(inline: Option<Secret>, file: Option<&std::path::Path>) -> Result<Secret, ferrohealth_secret::file::SecretError> {
    require_secret("cdr.password", inline, file)
}
```

Pin the version exactly, as every FerroHEALTH product does.

## A directory of named secrets

```rust,ignore
use std::time::Duration;

use ferrohealth_secret::dir::SecretDir;

fn sources() -> Result<(), ferrohealth_secret::dir::SecretDirError> {
    let dir = SecretDir::open("/run/secrets/sources")?;
    // Keep the watch for as long as the directory is read; dropping it stops the thread.
    let _watch = dir.watch(Duration::from_secs(10), |error| eprintln!("{error}"))?;
    let hmac = dir.get("partner-a-hmac")?;
    let client = dir.get_pair("partner-a-client")?;
    connect(hmac, client.certificate(), client.key());
    Ok(())
}
```

Each secret is one file named by the secret's name. A name is one path
segment with no leading dot, so a name from a database row cannot reach
outside the directory. `get_pair` reads `<name>.crt` and `<name>.key` from
the same update. Every read answers from one snapshot of the directory;
`refresh` takes a new one, and `watch` does so at a fixed interval, keeping
the last snapshot when a read fails. `dir::SecretDirConfig` holds the `dir`
and `poll_seconds` keys for a product's configuration file.

- **Kubernetes:** mount one Secret as a volume, without `subPath`. The
  kubelet updates the files when the Secret changes, after its sync period
  and cache delay; a `subPath` mount and an environment variable never
  update ([Secrets: using Secrets as files from a
  Pod](https://kubernetes.io/docs/concepts/configuration/secret/#using-secrets-as-files-from-a-pod)).
  The kubelet writes each update into a new hidden directory and renames
  the `..data` symlink to it, and the crate reads every key from the
  directory `..data` names, so a rotated certificate and key never mix.
  The External Secrets Operator or the Secrets Store CSI driver can fill
  the Secret.
- **Compose:** bind-mount a directory. Replace a secret by writing a new
  file beside it and renaming it over the old one; a rename is atomic, and
  a write in place can be read half done.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
