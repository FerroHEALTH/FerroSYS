<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-server

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-server-vX.Y.Z`.

## [Unreleased]

## [0.8.0] - 2026-11-09

### Removed

- Breaking: the named refusals of the `[server]` and `[server.tls]` keys
  0.7.0 retired are deleted, under the owner's rule that a removal aid
  lives for the one release that retires it (#328).
  `request_timeout_ms`, `header_read_timeout_ms`, `idle_timeout_ms`,
  `body_limit_bytes`, `drain_delay_ms`, `shutdown_timeout_ms`,
  `retry_after_s`, `compression_min_bytes` and `tls.reload_interval_s` are
  refused as unknown keys when the table loads, and the variants
  `section::ServerSectionError::Removed` and
  `section::ClientAuthError::Removed` are gone.

## [0.7.1] - 2026-11-09

### Added

- Feature `otlp`: `telemetry::build(&TelemetrySection, Service)` builds a
  product's telemetry with its OTLP collector dialed over the TLS that
  `tls_client::TlsClient` reads from `telemetry.otlp.tls`, over the
  platform verifier, and hands it to `Telemetry::with_tls`; with no
  `[telemetry.otlp]` it calls `Telemetry::new`. A product binary no longer
  writes that hand-over itself. `telemetry::TelemetryBuildError` names the
  TLS refusal and the telemetry refusal, each with its source (#323).

## [0.7.0] - 2026-11-09

### Changed

- Breaking: every span of time and amount of data in `[server]` is a
  `ferrohealth_config::units::Duration` or `ByteSize`, and its key names no
  unit. A product's file and environment move each key: `request_timeout_ms`
  to `request_timeout`, `header_read_timeout_ms` to `header_read_timeout`,
  `idle_timeout_ms` to `idle_timeout`, `body_limit_bytes` to `body_limit`,
  `drain_delay_ms` to `drain_delay`, `shutdown_timeout_ms` to
  `shutdown_timeout`, `retry_after_s` to `retry_after` (whole seconds,
  RFC 9110 §10.2.3), `compression_min_bytes` to `compression_min_size`, and
  `tls.reload_interval_s` to `tls.reload_interval`. Each old key is refused
  at load naming its replacement and the form of its value, all of them at
  once; the defaults keep their 0.6 values (#311).
- Breaking: `ServerSection`'s fields follow the keys, and its
  `request_timeout`, `header_read_timeout`, `idle_timeout`, `drain_delay`
  and `shutdown_timeout` methods are gone: read the field and call
  `Duration::get`. `ServerSection::body_limit_bytes` returns the limit as a
  `usize`. A zero `header_read_timeout`, `idle_timeout` or
  `tls.reload_interval`, a `retry_after` that is not whole seconds and a
  `body_limit` past the address space are refused at load
  (`section::ServerSectionError`, `section::ClientAuthError`) (#311).
- Breaking: `tls_client::TlsClient::read` reads
  `ferrohealth_config::tls::TlsClientSection`, and the listener's
  `TlsSection::min_version` is `ferrohealth_config::tls::TlsVersion` (#311).

### Added

- Feature `schema`: `schemars::JsonSchema` on `ServerSection` and every type
  it reaches, for a product that generates its configuration reference
  (#311).

### Removed

- Breaking: `tls_client::TlsClientSection`, `section::TlsVersion` and
  `section::TlsVersionError`, which live in `ferrohealth-config` 0.3 under
  `tls` (#311).

## [0.6.4] - 2026-11-09

### Added

- `tls_client::TlsClient` implements `PartialEq` and `Eq` over the
  material it read: the identity's chain and private key, the extra roots
  and the minimum version. The table key takes no part. A product that
  reloads its configuration compares two reads and rebuilds a client only
  when they differ, so a certificate renewed at the same path rebuilds it.
  `Debug` still shows no material (#305).
- `revocation::RevocationCheck::from_tls_client` builds the check from
  the material a `tls_client::TlsClient` read: its extra roots are the
  trust anchors, its identity is presented and its `min_version` is the
  link's minimum. A product that reads a table through `TlsClient` builds
  the check without reading the files a second time; `from_pem` shares the
  same build (#307).

## [0.6.3] - 2026-11-08

### Added

- `tls_client::TlsClient::certificate` returns the end-entity certificate
  of the client identity, the first certificate of its chain (RFC 8446
  §4.4.2), or `None` without an identity. A product that binds tokens to
  that certificate takes its RFC 8705 §3.1 thumbprint from it, without
  reading `client_identity_file` a second time (#300).

## [0.6.2] - 2026-11-08

### Added

- The stack sets the security headers of an API response outermost, so
  the request timeout `408`, the body limit `413`, the overload `503`,
  the panic `500` and the health routes carry them as well as a product's
  routes: `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: strict-origin-when-cross-origin`,
  `Cross-Origin-Resource-Policy: same-site`, `X-Frame-Options: DENY` and
  `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'`,
  from the OWASP Secure Headers Project. Each is set only when the response
  lacks it, so a route that sets its own policy or caching keeps it, and
  an operator console keeps every header of `console::headers`.
  `Strict-Transport-Security: max-age=31536000` is added only when the
  section's listener serves TLS itself (RFC 6797 §7.2). A route tree that
  answers with headers of its own design opts out with
  `api_headers::exempt`; the stack's refusals in front of it keep the set
  (#293).
- `tls_client::TlsClient::platform_config` (feature `platform-verifier`)
  gives the rustls configuration that verifies a server through the
  platform's verifier, with the extra roots of `trust_roots_file` beside the
  platform's own, and presents the client identity. It trusts what
  `TlsClient::apply` has reqwest trust, so a product that speaks TLS outside
  HTTP, such as MLLP, reaches the servers its HTTP client reaches, with the
  same protocol versions as `client_config` (#297).

## [0.6.1] - 2026-11-08

### Added

- `caller_rate::CallerLimit::take` is public: it takes one request of a
  caller's allowance at a given instant and returns the wait until the next
  when the request is over the rate, without counting it. `caller_rate::refusal`
  builds the `429 Too Many Requests` answer with `Retry-After` for that wait.
  The `limit` middleware is built on the two, so a product that decides on a
  caller's rate outside the middleware, right after its authentication,
  shares one state and one rule with it (#275).

## [0.6.0] - 2026-11-06

### Changed

- Breaking: the `revocation` feature builds on `der` 0.8, `x509-cert` 0.3
  and `x509-ocsp` 0.3 (`0.3.0-rc.0`, the only release on that line), so a
  product locks one copy of each beside FerroFED's. The `der::Error` that
  `RevocationSetupError::RootDer`, `RevocationError::Certificate`,
  `StatusError::Decode` and `SignatureError::Encoding` carry is now der 0.8's.
  A product that matches on its kind names der 0.8's `ErrorKind`. The
  check gains der 0.8's parsing: a cap of 64 nested messages, a
  `SEQUENCE` required by `Document::decode`, and no stack overflow on
  nested BER indefinite lengths. An OCSP answer with trailing data inside
  a nested value still fails closed as `StatusError::Decode` (#251).
  `tokio-postgres-rustls` 0.14.0, its newest release, still requires
  `x509-cert` 0.2, so a product that uses it keeps a second `x509-cert`
  and `der` line until that crate moves.

## [0.5.1] - 2026-11-05

### Added

- `revocation::RevocationCheck` (feature `revocation`), the TLS of an
  outbound link that checks every certificate a peer presents for
  revocation before any data is sent. `RevocationCheck::clear` opens a
  handshake that sends nothing, pairs each certificate with its issuer up
  to one of the trust roots it was given, and asks the certificate's CRL
  distribution points (RFC 5280 §6.3), then its OCSP responders (RFC
  6960). A revoked certificate fails the check, and so does one no source
  gives a status for: a source that cannot be reached, a stale CRL, a CRL
  with a critical extension the check does not process, or an OCSP status
  of `unknown`. `RevocationCheck::http_client` is a reqwest client that
  completes a handshake only over a chain a check cleared in the last five
  minutes (#243).

## [0.5.0] - 2026-11-05

### Added

- Breaking: `refusal::DEFAULT_BODY` is public, the plain-text refusal body
  a product that names none answers with. A product takes it with
  `Arc::clone(&refusal::DEFAULT_BODY)`, and the stack still tells it from
  a product's own body by pointer (#231).

### Removed

- Breaking: five public functions that only forwarded to another call are
  gone, and a product calls the target itself (#231):
  - `problem::ProblemType::problem(&self)`: call
    `problem::Problem::of(&kind)`.
  - `refusal::default_body()`: take
    `Arc::clone(&refusal::DEFAULT_BODY)`.
  - `same_origin::SameOrigin::new()`: call `SameOrigin::default()`.
  - `tls::server_config(section)`: call
    `tls::server_config_of(section, TlsKeys::SERVER)`.
  - `tls::HealthcheckTls::read(section)`: call
    `HealthcheckTls::read_of(section, TlsKeys::SERVER)`.

## [0.4.4] - 2026-11-05

### Changed

- The serve path, the stack and the TLS setup split by concern into
  private modules, each duplicated block to one owner, with the public
  API and the behaviour unchanged (#213, #214, #215, #220).

## [0.4.3] - 2026-11-03

### Added

- `run::Run`, the run path a binary's `main` hands its parser and its
  dispatch: it parses the command line (help and the version exit 0, a
  usage error exits with clap's code), starts the multi-threaded runtime
  with every driver on (`Run::thread_stack_size` sets its threads' stack),
  runs the product's job on it and returns its exit code. A job that fails
  returns a `run::Failure`, which any `Error` converts into with `?` and an
  `anyhow::Error` or a message through `Failure::boxed`; it is written to
  standard error as `<name>: <context>: <cause chain>` and exits 1, or the
  code `Failure::with_code` names, such as `run::EXIT_CONFIG` (78,
  `EX_CONFIG`). `run::chain` renders an error and every cause behind it on
  one line from each cause's `Display` alone. A product keeps its own
  subcommands and their bodies and drops its own parse, runtime builder and
  chain renderer; a job run under `Run` starts no runtime of its own (#208).

## [0.4.2] - 2026-10-30

### Added

- `console::ConsoleHeaders::server_rendered`, the console headers of a
  console drawn on the server with no inline script, no inline style and
  no WebAssembly, such as one on FerroUI 0.6: `script-src 'self'` and
  `style-src 'self'`, with no nonce and no `'wasm-unsafe-eval'` (W3C
  Content Security Policy Level 3, §6.1.1). `console::headers` mints no
  nonce for it and puts no `CspNonce` in the request. Under its bundle
  path an asset keeps the `Cache-Control` its handler set, such as the
  one-year `immutable` of FerroUI's asset router (RFC 9111 §5.2.2), and a
  refusal is `no-store` whatever it set. A console that writes its own
  policy over the header takes `ConsoleHeaders::server_rendered` in the
  place of `ConsoleHeaders::new` and drops that layer.
  `ConsoleHeaders::new` and its nonce policy are unchanged (#202).

## [0.4.1] - 2026-10-29

### Fixed

- `stack::app` and `Stack::into_router` keep the product's default fallback
  with every layer the product applied, a regression since 0.3.10: merging
  the routes beside the base path, empty through `stack::app`, replaced it
  with a bare one, so a path no route matched was answered past the
  product's layers (a guard answered `404` where it answered `401` before).
  Routes beside that declare no fallback now keep the product's; routes
  beside that declare one still answer with theirs (#199).

## [0.4.0] - 2026-10-29

### Added

- Breaking: `[server] idle_timeout_ms` (`ServerSection::idle_timeout_ms`,
  default 120000) closes a kept-alive HTTP/1.1 connection that waits that
  long for its next request, and `serve::IDLE_TIMEOUT` is the bound of
  `serve::serve` and `serve::serve_tls`. The default outlasts the 90 s Go's
  `http.DefaultTransport` keeps an idle connection, so a client normally
  closes first. A product that builds `ServerSection` with a struct
  literal and no `..ServerSection::default()` names the new field (#193).
- `healthcheck::Get`, a `GET` to a server on this host over plain HTTP or
  over the healthcheck's pinned TLS (`tls::HealthcheckTls`), with request
  headers, that answers `healthcheck::Answer`, the status and the body. The
  caller bounds the body and the whole exchange; past either, and for a
  connection, a handshake or an answer that fails, it answers a typed
  `healthcheck::GetError`. A product reads its own readiness, operator or
  metrics documents through it with no TLS client of its own.
  `healthcheck::check`, `check_tls` and `check_url` run the same exchange
  and answer as before (#195).

### Changed

- Breaking: a `panic::PanicBody` and a `refusal::RefusalBody` take
  `request_id::RequestIds` in place of the server's request id: `server`,
  the id the request log and the panic line name, and `echoed`, the id the
  response echoes in `X-Request-Id` (the client's legal id when it sent
  one, the server's otherwise). A product whose error bodies name the id
  the client sees writes `ids.echoed`; the client's id goes back only to
  the client that sent it and is still never logged. A body written for
  the server's id keeps its closure through `panic::from_server_id` and
  `refusal::from_server_id`. `problem::panic_body`, `problem::refusal_body`
  and the default bodies keep naming the server's id (#191).

### Fixed

- On a kept-alive HTTP/1.1 connection, `header_read_timeout_ms` counts from
  the first byte of each request head. It used to count from the end of
  the previous response, so it doubled as a 30 s idle limit and the server
  closed the connection without an answer when a request head was still
  arriving as it passed: a client that reused the connection then met a
  closed socket, which a `POST` cannot safely retry (RFC 9112 §9.3.1). An
  idle connection is now closed after `idle_timeout_ms` with a clean end of
  stream, and a head that takes longer than the bound from its own first
  byte is still refused. HTTP/2 and upgraded connections are left to their
  own protocol (#193).

## [0.3.10] - 2026-10-29

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

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.7.1...HEAD
[0.7.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.7.0...ferrohealth-server-v0.7.1
[0.7.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.6.4...ferrohealth-server-v0.7.0
[0.6.4]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.6.3...ferrohealth-server-v0.6.4
[0.6.3]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.6.2...ferrohealth-server-v0.6.3
[0.6.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.6.1...ferrohealth-server-v0.6.2
[0.6.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.6.0...ferrohealth-server-v0.6.1
[0.6.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.5.1...ferrohealth-server-v0.6.0
[0.5.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.5.0...ferrohealth-server-v0.5.1
[0.5.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.4.4...ferrohealth-server-v0.5.0
[0.4.4]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.4.3...ferrohealth-server-v0.4.4
[0.4.3]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.4.2...ferrohealth-server-v0.4.3
[0.4.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.4.1...ferrohealth-server-v0.4.2
[0.4.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.4.0...ferrohealth-server-v0.4.1
[0.4.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.10...ferrohealth-server-v0.4.0
[0.3.10]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-server-v0.3.9...ferrohealth-server-v0.3.10
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
