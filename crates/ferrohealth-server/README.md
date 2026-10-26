<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-server

The HTTP server plumbing of the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). Every server and console
runs the same stack, the same health routes and the same shutdown.

## What it does

- `stack::app` wraps a product's router, under its base path, in:
  - the request id: the server mints its own and logs it; a client's
    `X-Request-Id` is echoed and never logged;
  - the panic catch: a `500` with the body the product supplies, and no
    panic message in the body or the log;
  - the request timeout (`408`) and the body limit (`413`);
  - the overload bound: past `max_concurrent_requests` a request is answered
    `503` with `Retry-After`, and the health routes are never refused;
  - the request log of `ferrohealth-telemetry`.
- `health::routes` serves `/health`, `/health/liveness` and
  `/health/readiness` over `ferrohealth-health`.
- `serve::serve` runs until `SIGTERM` or `SIGINT`. It then withdraws
  readiness, waits the drain delay, and gives the requests in flight a
  bounded time to finish.
- `healthcheck::check` is the container probe: readiness over loopback,
  exit `0` only on `200`.
- `cli::Common` holds the `serve`, `config check` and `healthcheck`
  subcommands every binary flattens into its own, and `cli::ConfigArgs`
  the `--config` and `--set` arguments.
- `section::ServerSection` is the `[server]` section, with the listener's
  TLS (mutual TLS, reload on `SIGHUP`), the trusted proxies, the caller
  rate and the compression.
- `serve::run_all` serves several listeners under one signal,
  `client_address` names a request's client, `caller_rate` limits one
  caller, `health::Health::with_info` serves `/health/info`, and
  `tls_client` reads the TLS material of outbound calls (feature `reqwest`
  sets it on a reqwest client).
- `console` (feature `console`) adds an operator console's nonce
  Content-Security-Policy and the security headers beside it.

## Use

```toml
[dependencies]
ferrohealth-server = "=0.2.0"
```

Install `ferrohealth_telemetry::panic::install_hook` at start, so Rust's own
hook prints no panic message. Pin the version exactly, as every FerroHEALTH
product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
