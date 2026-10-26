<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-telemetry

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-telemetry-vX.Y.Z`.

## [Unreleased]

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
