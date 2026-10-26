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
- `provenance::Provenance` says where each key came from: the default, the
  file or the environment, with the variable.
- `report::check_report` renders the running tree for `config check`, one
  key per line with its source. Secrets held in the redacting types of
  `ferrohealth-secret` show as `***`, and a secret's `<key>_file` sibling is
  read with `ferrohealth_secret::file`.

## Use

```toml
[dependencies]
ferrohealth-config = "=0.2.5"
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
