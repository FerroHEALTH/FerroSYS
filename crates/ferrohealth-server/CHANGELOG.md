<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-server

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-server-vX.Y.Z`.

## [Unreleased]

### Added

- `[server.tls]` and `serve::serve_tls`: the listener serves its own
  certificate chain and key over TLS 1.3 and 1.2 with HTTP/2 and HTTP/1.1 by
  ALPN, the key read through `ferrohealth-secret`, each handshake bounded,
  and the same bounded drain as plain HTTP; `serve::run` picks the one the
  section configures (#32).
- `healthcheck::check_tls` and `tls::HealthcheckTls`: the container probe
  speaks TLS to a TLS listener over loopback and accepts exactly the leaf
  certificate the listener's file holds (#32).
- `build_info::BuildInfo` and `build_info::banner`: the commit, build
  instant and compiler a product's build script records, and the boot banner
  printed only for the `pretty` console (#32).

## [0.1.0] - 2026-10-26

### Added

- `stack::app`, the stack every server runs under its base path: the
  server's own request id, a client's `X-Request-Id` echoed and never
  logged, the panic catch with the product's `500` body and no panic message
  anywhere, the request timeout (`408`), the body limit (`413`), the
  overload bound (`503` with `Retry-After`) that never refuses the health
  routes, and the request log (#17).
- `health::routes`: `/health`, `/health/liveness` and `/health/readiness`
  (#17).
- `serve::serve` and `serve::shutdown_signal`: on `SIGTERM` or `SIGINT`
  readiness turns `503`, the drain delay passes, and the drain is bounded
  (#17).
- `healthcheck::check`, the container probe over loopback (#17).
- `cli::Common`, the `serve`, `config check` and `healthcheck` subcommands
  as one flattenable `clap` enum (#17).
- `section::ServerSection`, the `[server]` section, with the listen address,
  the base path and the bounds refused at load when invalid (#17).
- `console` (feature `console`): the nonce Content-Security-Policy and the
  security headers of an operator console (#17).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.1.0...HEAD
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-server-v0.1.0
