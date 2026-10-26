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
- `[server.tls] client_auth` (`none`, `optional`, `required`) with
  `client_ca_file`: mutual TLS, where a presented client chain is verified
  against the named CA and reaches the handler as the
  `tls::PeerCertificates` request extension; a `client_auth` without a CA is
  refused at load (#39).
- `tls::ReloadableTls`: the certificate and key are read again on `SIGHUP`
  and every `reload_interval_s`, a reload that does not read keeps the
  running configuration, and the next handshake takes the new files (#40).
- `[server.tls] healthcheck_identity_file` and `healthcheck::check_url`: the
  probe presents a client identity to a listener that requires one, and
  checks a full `http://` or `https://` URL (#42).
- `serve::run_all` and `serve::Listener`: several listeners, each with its
  own routes and TLS, served under one signal and one lifecycle and drained
  together within one bound (#43).
- `client_address`: `[server] trusted_proxies` and `forwarded_header`; the
  stack puts the `ClientAddress` of every request in its extensions, read
  from `Forwarded` (RFC 7239) or `X-Forwarded-For` only behind a trusted
  proxy, and the peer address otherwise (#44).
- `caller_rate`: `[server.caller_rate]` and the `limit` middleware a product
  mounts inside its authentication, keyed by the `CallerKey` it sets; a
  caller over its rate is answered `429` with `Retry-After` (#45).
- `[server] compression` (on by default) and `compression_min_bytes`: gzip
  and Brotli by `Accept-Encoding` (#46).
- `Health::with_info`: `GET /health/info` with the build facts and the
  product's own (#47).
- `cli::ConfigArgs`: `--config` and the repeatable `--set key=value` every
  binary shares (#49).
- `ConsoleHeaders::with_bundle`: console documents are `Cache-Control:
  no-store` and the site bundle `no-cache` (#50).

### Changed

- TLS 1.2 offers only the four ECDHE AES-GCM suites RFC 9325 §4.2
  recommends; TLS 1.3 keeps every suite (#41).

### Fixed

- The request log is mounted outside the routes, so a request no route
  matched (a `404`) also writes its one line, named `<unmatched>` (#70).

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
