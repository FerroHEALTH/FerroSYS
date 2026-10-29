<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-telemetry

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-telemetry-vX.Y.Z`.

## [Unreleased]

### Added

- The panic line of `panic::install_hook` carries `request_id`, the
  server's id of the request the panicking thread is serving, when the
  request log (feature `axum`, as `RequestLog` layer or `middleware`) wraps
  that request and the request carries a `request_log::RequestId`. A panic
  outside a request, or in a request with no id, leaves the field off. The
  id is the server's own, so nothing a client wrote reaches the line, and
  the panic message stays withheld. A product with a hook of its own for
  the request id can drop it for this one. A task the handler spawns runs
  outside the request and names none (#181).

## [0.2.3] - 2026-10-29

### Added

- `request_log::ValueShape` and `RequestLog::with_query_shape` and
  `RequestLog::with_default_query_shape`: a product can require a logged
  query value to be one to `max` ASCII digits, per parameter or for the
  whole allow-list, and a value of another shape is left out of the line
  with its key, so `offset=<an identifier>` never reaches it. With no shape
  set, `[telemetry] logged_query_parameters` logs any text cut to 64
  characters, as before (#167).
- `RequestLog::with_metrics` also records `http.server.active_requests`
  (`request_log::ACTIVE_REQUESTS`), the up-down counter of the requests in
  flight by `http.request.method` and `url.scheme` (OpenTelemetry semantic
  conventions v1.44.0, `docs/http/http-metrics.md`). A request is counted
  in when it begins and out when its future completes or is dropped, so a
  request whose client goes away is counted out too. A product that calls
  `with_metrics` gains the series `http_server_active_requests` (#169).

### Changed

- The request span is a server span named `{method} {route}` (`HTTP
  {route}` for a method outside the known set, the method alone when no
  route matched or was named), and its status is `Error` for a 5xx, under
  the OpenTelemetry semantic conventions v1.44.0 (`docs/http/http-spans.md`,
  Name and Status), read by `tracing-opentelemetry` from `otel.name`,
  `otel.kind` and `otel.status_code`. The `tracing` span keeps the name
  `request`; a product test that reads the exported span's name `request`
  reads the new name (#166).
- `Telemetry::new` refuses `telemetry.otlp.trust_roots_file` or
  `telemetry.otlp.client_identity_file` beside an endpoint that is not
  `https://` (`InitError::TlsWithoutHttps`, naming the key), where it read
  the table and exported in plain text with the files ignored. It also
  refuses a trust roots file that holds no PEM certificate and a client
  identity file that lacks a PEM certificate or a PEM private key
  (`InitError::TlsPem`, naming the key and the path and quoting no part of
  the file), where it handed the file to tonic and failed, or did not, at
  the first export. Migration: point the endpoint at `https://` when the
  collector speaks TLS, or drop the two keys when it does not; and fix a
  file the load now refuses (#168).

### Fixed

- A method outside the known set is `_OTHER` in the request line's
  `method` and the span's `http.request.method`, as it already was in the
  histogram; the client's token reaches neither (#166).

## [0.2.2] - 2026-10-27

### Added

- `request_log::RequestLog` is a `tower::Layer`, whose
  `RequestLogService` writes the line, the span and the sample
  `request_log::middleware` writes, with no boxed future (#129).

### Changed

- A clone of `RequestLog` or `RequestId` shares its contents in place of
  copying them, so a stack that clones its layers for every request pays no
  allocation for either, and the request log copies neither the route
  template nor the request id per request (#129).

## [0.2.1] - 2026-10-26

### Changed

- Requires `ferrohealth-secret` as `0.2.0`, caret requirements in place of exact pins, so a product takes a patch of those crates without waiting for a release of this one.

## [0.2.0] - 2026-10-26

### Added

- `telemetry::Console`, how the console is drawn: `NO_COLOR` set to any
  non-empty value turns colour off (<https://no-color.org/>), and
  `Console::force` names directives that write whatever the configured
  filter says, such as a security log at `info` (#53, #54).
- `telemetry::FilterHandle`, returned by `Telemetry::install` and in
  `telemetry::Layers`, replaces the configured console filter at run time,
  refuses a directive that does not parse, keeps the forced directives and
  reads the running filter back (#55).
- `[telemetry] environment` sets `deployment.environment.name` on the
  resource, and `Service::environment` sets it in code (#56).
- `RequestLog::with_metrics` records every request in the
  `http.server.request.duration` histogram (seconds, the advisory buckets)
  of the OpenTelemetry HTTP semantic conventions, with
  `http.request.method`, `url.scheme`, `http.response.status_code`,
  `http.route`, `network.protocol.version` and, on a server error,
  `error.type`; `RequestLog::serving_tls` sets `url.scheme` to `https`
  (#51).
- `request_log::fields::LineFields`, a response extension whose fields the request
  log writes as named fields of the line, each a lower-case name and a
  short visible-ASCII `Token` the product vouches for (#52).
- `request_log::RouteNamer`, set with `RequestLog::with_route_namer`,
  names the route of a request no route matched, for the line, the span,
  the histogram and the recorders (#52).
- `telemetry::prometheus_route` (features `axum` and `prometheus`) answers
  `GET` with the metrics in the Prometheus text format under
  `text/plain; version=0.0.4` (#57).

### Changed

- `Telemetry::layers` and `Telemetry::install` take a `Console` in place
  of the terminal flag; `layers` returns `Layers` and `install` returns the
  `FilterHandle` (#53, #54, #55).

## [0.1.0] - 2026-10-26

### Added

- `section::TelemetrySection`, the `[telemetry]` section: the console
  format, a `Filter` refused at load when it does not parse, the query
  parameters the request log may carry, and the OTLP collector with its
  sample ratio and TLS files (#15).
- `telemetry::Telemetry`, the one OpenTelemetry provider of a process: one
  meter provider read by Prometheus (feature `prometheus`) and pushed over
  OTLP (feature `otlp`), and one tracer provider that exports only the
  product's own spans; no propagator is installed, so a client's
  `traceparent` never parents a server span (#15).
- `console`, the `json` and `pretty` renderings, each record kept on one
  line (#15).
- `panic::install_hook`, one fixed line per panic that withholds the
  message (#15).
- `request_log` (feature `axum`): one line and one span per request with
  the route template and the server's request id, and never a body, the raw
  path, a header value, an unlisted query value or the client's request id;
  a `Recorder` sees every finished request (#15).

[Unreleased]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-telemetry-v0.2.3...HEAD
[0.2.3]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-telemetry-v0.2.2...ferrohealth-telemetry-v0.2.3
[0.2.2]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-telemetry-v0.2.1...ferrohealth-telemetry-v0.2.2
[0.2.1]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-telemetry-v0.2.0...ferrohealth-telemetry-v0.2.1
[0.2.0]: https://github.com/FerroHEALTH/FerroSYS/compare/ferrohealth-telemetry-v0.1.0...ferrohealth-telemetry-v0.2.0
[0.1.0]: https://github.com/FerroHEALTH/FerroSYS/releases/tag/ferrohealth-telemetry-v0.1.0
