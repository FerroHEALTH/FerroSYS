<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-config

The configuration loader of the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). Every product and
console reads its configuration the same way, so an operator learns it once.

## What it does

- `loader::Loader` reads one serde tree from three layers: the tree's
  defaults, an optional TOML file (named by the caller or by
  `<PREFIX>_CONFIG`), and the environment, where `<PREFIX>__SECTION__KEY`
  sets `[section] key` and a product's aliases (such as `RUST_LOG`) set the
  keys they name. An override is placed as text, and typed only where the
  tree wants a number, a boolean or an array.
- An array of tables, such as `[[auth.issuer]]`, is set by element index:
  `<PREFIX>__AUTH__ISSUER__0__URL` sets `url` of the first element, and
  `--set auth.issuer.0.url=…` or `--set auth.issuer[0].url=…` does the
  same. An index overrides the file's element key by key, and the next
  index past the end adds one. The indices number the elements from 0
  without a gap; a skipped element (`ConfigError::OverrideGap`), an index
  with a leading zero (`ConfigError::OverrideIndex`) and an index under a
  key that holds no array of tables (`ConfigError::OverrideArray`) are
  refused. An element is read as strictly as the file, `<key>_file`
  secrets included.
- `Loader::fallback_composed` sets a key from several variables at once,
  and `libpq::url` is the composer of a database URL from PostgreSQL's
  client environment (`PGHOST`, `PGUSER`, `PGPASSWORD` and the rest), the set
  managed PostgreSQL services inject in place of a URL.
- The load is strict. With `#[serde(deny_unknown_fields)]` on every struct,
  an unknown key and a value of the wrong type are refused with the dotted
  key, the variable that set it and, for an unknown key, the closest known
  one. Only the override form `<PREFIX>__…` is checked against the tree;
  every other variable under the prefix is left alone, because product
  prefixes nest (`FERROTERM` and `FERROTERM_CONSOLE`). No refusal quotes a
  value.
- `Loaded::table_with_secrets` is the table the sources set, the typed
  overrides applied and the defaults left out, for a product
  that stores its configuration as revisions. It holds secrets as read.
- `provenance::Provenance` says where each key came from: the default, the
  file or the environment, with the variable; `Provenance::secret_source`
  counts a secret and its `<key>_file` sibling as one setting.
- `report::check_report` renders the running tree for `config check`, one
  key per line with its source. Secrets held in the redacting types of
  `ferrohealth-secret` show as `***`, and a secret's `<key>_file` sibling is
  read with `ferrohealth_secret::file`.
- `units::Duration` and `units::ByteSize` are the one way every section
  writes a span of time and an amount of data: a whole number and its unit
  with no space, `"30s"`, `"10min"`, `"7d"` (UCUM codes `ms`, `s`, `min`,
  `h`, `d`) and `"64MiB"` (IEC 80000-13 binary units). A bare number, `m`
  and a decimal unit such as `MB` are refused, each naming the right form.
- `removed::Removed` keeps a retired key readable so that
  `removed::Removals` can refuse every retired key a file sets, all at
  once, each naming its replacement. A key is never read under two names.
  Each section declares its retired keys as `removed::Retired` with the
  release that retired them, and one unit test runs `removed::stale` over
  them with the release being prepared (the crate's version, or the
  product's next release when its version moves only at the cut), so a
  refusal is deleted in the next release: a removal aid lives for one
  release.
- `tls::TlsClientSection` is the TLS table of every outbound call, read by
  `ferrohealth-server`'s `tls_client::TlsClient`, and `tls::TlsVersion` the
  oldest TLS version a listener or a client speaks.
- Feature `schema` derives `schemars::JsonSchema` on these types, for a
  product that generates its configuration reference.

## Use

```toml
[dependencies]
ferrohealth-config = "=0.4.4"
```

```rust
use ferrohealth_config::loader::Loader;

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    listen: String,
}

fn load() -> Result<Config, ferrohealth_config::error::ConfigError> {
    let loader = Loader::new("FERROSYS")?.alias("RUST_LOG", "telemetry.filter");
    Ok(loader.load::<Config>(None)?.into_parts().0)
}
```

Pin the version exactly, as every FerroHEALTH product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
