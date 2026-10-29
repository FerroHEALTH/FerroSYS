<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-server

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-server-vX.Y.Z`.

## [Unreleased]

### Added

- `stack::Stack`, the builder behind `stack::app`: `Stack::beside` mounts
  routes at the root beside the base path, and `Stack::fallback` answers
  every request no route matched with the product's handler, such as a
  `404` body of its own for a path outside the base path. The request id,
  the request log, the client address, the refusal and panic bodies, the
  timeout, the body limit and the compression cover both; the overload
  bound covers the routes beside and leaves the fallback unbounded. The
  health routes stay under the base path. `stack::app` is
  `Stack::new(..).into_router()` and serves as before (#180).

## [0.3.9] - 2026-10-29

### Fixed

- The refusal of a client CA under `client_auth = "none"` names `none`,
  `required` and `optional` without quotes, so it reads in full after
  `ferrohealth-config` renders a configuration error, which masks every
  quoted value (#174).

## [0.3.8] - 2026-10-28

### Added

- `problem` writes RFC 9457 problem details as `application/problem+json`
  for every error body a product answers outside a FHIR route. A `Problem`
  carries the `type`, `title`, `status`, `detail` and `instance` members
  and the extension members of a problem type, and answers its status as
  an axum response; `ProblemType::about_blank` is the type of a problem
  that adds nothing to its status. A product defines its types once in a
  `ProblemTypes` registry under a base URI it owns and documents, which
  refuses a slug defined twice. `problem::refusal_body` and
  `problem::panic_body` are the opt-in for the stack's own `408`, `413`,
  `429`, `503` and panic `500`: set in a `StackPolicy`, they write those as
  problems with a fixed detail and the request id as a `urn:uuid:`
  instance, never the path or a panic's message. A product that does not
  opt in keeps its bodies and `refusal::keep_status` (#160).

- A product can keep a listener's TLS and reload it from its own reload
  path: `serve::run_with_tls` and `serve::Listener::with_reloadable_tls`
  serve a `tls::ReloadableTls` the product built, and its `reload` returns
  the outcome, so a configuration change whose renewed pair does not read
  is refused with it. The server still reads the same configuration again
  on `SIGHUP` and every `reload_interval_s` (#159).

- `tls::TlsKeys`, with `tls::TlsKeys::SERVER` and the `tls_keys!` macro,
  names the table a listener's TLS comes from: `tls::server_config_of`,
  `tls::ReloadableTls::of` and `tls::HealthcheckTls::read_of` name every
  fault under its keys, so a second listener configured from
  `[metrics.tls]` reports `metrics.tls.certificate_file` where it reported
  `server.tls.certificate_file`. A client CA bundle rustls refuses is
  `TlsError::ClientCaRefused` with its key; `TlsError::ClientCa` is
  deprecated and no longer returned (#159).

### Changed

- A `[server.tls]` table that sets `client_ca_file` while `client_auth` is
  `none`, the default included, is refused when the configuration is read,
  and `tls::server_config` refuses such a `TlsSection` built in code with
  `TlsError::ClientAuth`. Before, the CA was read and ignored and the
  listener asked no client certificate, so a deployment that meant mutual
  TLS served without it (RFC 8446 §4.3.2: a client sends a certificate only
  after the server's CertificateRequest). Migration: set `client_auth =
  "required"` (or `"optional"`) beside `client_ca_file`, or remove the file.
  A section built in code with `client_auth` set and no CA is refused there
  too, as the configuration already refused it (#158).

## [0.3.7] - 2026-10-28

### Fixed

- `caller_rate::CallerLimit` no longer refuses requests under concurrency
  when `requests_per_second` is above 1 000 000 000. The interval of such a
  rate truncated to zero, which left no tolerance for a request whose clock
  read came before a concurrent one's. A rate the nanosecond clock cannot
  resolve now limits nothing; one request per nanosecond still limits as
  before (#149).
- An HTTP/1.1 response whose request body the routes left unread, such as a
  `404` to a `POST` of an unserved path or a declared-length `413`, now
  carries `Connection: close`. The connection closes after such a response,
  and a client that reused it for its next request met a closed socket,
  which a non-idempotent request cannot safely retry (RFC 9112 §9.6). A
  request whose body was read to its end keeps its connection (#150).

### Security

Shipped with functional changes because: the two fixes beside it are in the same serving path, and every product takes `ferrohealth-server` by one exact pin in one lockstep pull request, so a security-only 0.3.7 would be followed by a 0.3.8 for the same products within the day.

- A plain-HTTP connection that never sends a byte is closed once the header
  read timeout passes, counted from accept (`[server]
  header_read_timeout_ms`, 30 s by default). Before, hyper-util's protocol
  detection waited for the first byte with no deadline, so a client with no
  credentials that opened a few thousand silent connections held every file
  descriptor of the process and the service stopped answering, health
  checks included. The same bound now holds after a TLS handshake. After an
  `accept` error that is not one connection's, such as `EMFILE` or
  `ENFILE`, the listener pauses from 5 ms doubling up to 1 s before it
  accepts again, and logs at most one warning every 10 seconds with the
  count it suppressed, where it used to retry at once and log each failure
  (#151, GHSA-2mc2-h3jq-c5wg).

## [0.3.6] - 2026-10-27

### Added

- `refusal::keep_status` lets a product's `RefusalBody` answer a status of
  its own, such as a FHIR gateway's `504 Gateway Timeout` with a `timeout`
  `OperationOutcome` where the stack refuses a timeout `408`; the stack still
  adds the refusal's `Retry-After` and the request id header. A body that
  does not call it answers the stack's status, as before (#146).

### Fixed

- Every request leaves one request line, with its method, route, status,
  latency and the server's request id: `stack::app` now runs the request
  log outside the edge, so a timeout `408`, a declared-length `413` and the
  `500` of a panicked handler are logged too (#145).
- A `413` a body extractor raises because a body with no declared length
  passed the limit is written with the product's `RefusalBody`, as the
  declared-length `413` is, so a FHIR product answers it with an
  `OperationOutcome`. Under the default body it keeps the extractor's own
  text (#146).

## [0.3.5] - 2026-10-27

### Added

- A product states its own `ServerSection` defaults, such as listening on
  `0.0.0.0` or a larger body ceiling, with `Loader::defaults` of
  `ferrohealth-config` 0.2.6; a partial `[server]` table then keeps the
  product's value for every key it does not name, and an operator's value
  wins over it. The section's documentation says how (#136).
- `Health::registry` returns the `Registry` the health routes read, beside
  `Health::lifecycle`, so a product builds its registry and lifecycle once
  and reads the readiness with its detail from the same indicators the
  routes serve (#135).

### Fixed

- `serve::run`, `serve::run_all`, `serve::serve` and `serve::serve_tls` set
  `TCP_NODELAY` on every accepted connection, plain and TLS, so a small
  response no longer waits on Nagle's algorithm and the client's delayed
  acknowledgment (#137).
- The handler of an HTTP/2 request still running when the drain bound
  passes is cancelled before `serve::run` and the other serve calls return,
  as an HTTP/1.1 handler already was: hyper runs HTTP/2 handlers on tasks of
  their own, which now live in a set the drain aborts and waits for (#131).

## [0.3.4] - 2026-10-27

### Fixed

- `stack::app` costs about a quarter of what it did per request: 1.4 µs
  in place of 5.2 µs to an empty handler on the crate's new `stack`
  benchmark, beside a bare axum floor of 0.4 µs. The request id, the client
  address, the panic catch, the request timeout, the declared body limit and
  the rendering of refusals and panics run as one service with one unboxed
  future, the layers are composed once around each route, the timeout's
  timer is armed only once a handler waits, and the request id comes from a
  thread-local generator in place of one system call per request. Every
  answer, header and log line is as before (#129).

### Changed

- Requires `ferrohealth-telemetry` 0.2.2, whose `RequestLog` layer the
  stack runs in place of `request_log::middleware` (#129).

- `serve::run`, `serve::run_all`, `serve::serve` and `serve::serve_tls`
  close every connection still open when the drain bound passes, before
  they return `Drained::Cut`, as documented; the connection tasks kept
  running until the runtime ended (#123).

## [0.3.3] - 2026-10-26

### Fixed

- `console::headers` sets `no-cache` under the bundle path only on a
  successful or not-modified answer; a refusal there, such as the 404 for a
  bundle file that does not exist, is `no-store` like every other response
  (#120).

## [0.3.2] - 2026-10-26

### Changed

- Requires `ferrohealth-secret` as `0.2.0`, `ferrohealth-telemetry` as `0.2.0` and `ferrohealth-health` as `0.1.1`, caret requirements in place of exact pins, so a product takes a patch of those crates without waiting for a release of this one.

## [0.3.1] - 2026-10-26

### Added

- `same_origin` (feature `console`): the layer that refuses a request whose
  method is not safe unless `Sec-Fetch-Site` is `same-origin`, or without
  it `Origin`, or without that the origin of `Referer`, shows the console's
  own origin; the refusal is a `403` in the product's `RefusalBody`,
  marked `no-store` and carrying the typed `CrossOrigin` reason (#100).

## [0.3.0] - 2026-10-26

### Added

- `StackPolicy::refusal_body` and the `refusal` module: the refusals the
  stack writes itself (`408` past the request timeout, `413` for a declared
  body past the limit, `429` past a caller's rate, `503` past the overload
  bound) are written in the product's body, keeping the status and
  `Retry-After`; a response a handler wrote is never rewritten (#91).
- `[server] header_read_timeout_ms` (default 30000): a client that does not
  finish its request head in time is disconnected, on the plain and the TLS
  listener alike (#91).
- `[server.tls] min_version` and the same key on `tls_client::TlsClientSection`
  (`section::TlsVersion`): `"1.3"` (RFC 8446) by default, `"1.2"` where a
  deployment lowers it (RFC 9325 §3.1.1), and `"1.0"` or `"1.1"` refused at
  load as deprecated (RFC 8996) (#91).

### Changed

- The listener and the outbound TLS client speak TLS 1.3 alone unless
  `min_version = "1.2"` is set; they offered 1.2 and 1.3 before (#91).
- `/health/readiness` serves each indicator's name and state without its
  detail, which can name a dependency or a count and is open to anyone who
  reaches the port (#91).
- The plain listener is served by the same connection loop as the TLS
  listener, so both bound the request head and both put the connection's
  address on each request (#91).

### Fixed

- A TLS listener no longer builds its route table again for every
  connection: the connection's address and client certificates ride on each
  request to the one router (#85).

## [0.2.0] - 2026-10-26

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
- `tls_client::TlsClientSection` and `tls_client::TlsClient`: the outbound
  TLS keys `client_identity`, `client_identity_file` (through
  `ferrohealth-secret`) and `trust_roots_file`, read once into a rustls
  `ClientConfig` over the caller's platform roots, or set on a
  `reqwest::ClientBuilder` behind the `reqwest` feature (#48).

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

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.9...HEAD
[0.3.9]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.8...ferrohealth-server-v0.3.9
[0.3.8]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.7...ferrohealth-server-v0.3.8
[0.3.7]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.6...ferrohealth-server-v0.3.7
[0.3.6]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.5...ferrohealth-server-v0.3.6
[0.3.5]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.4...ferrohealth-server-v0.3.5
[0.3.4]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.3...ferrohealth-server-v0.3.4
[0.3.3]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.2...ferrohealth-server-v0.3.3
[0.3.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.1...ferrohealth-server-v0.3.2
[0.3.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.0...ferrohealth-server-v0.3.1
[0.3.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.2.0...ferrohealth-server-v0.3.0
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.1.0...ferrohealth-server-v0.2.0
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-server-v0.1.0
