<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-telemetry

The telemetry of the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS): one console, one request
log and one OpenTelemetry provider, so every product logs and exports the
same way.

## What it does

- `section::TelemetrySection` is the `[telemetry]` section: the console
  format (`auto`, `json`, `pretty`), the filter, the query parameters the
  request log may carry, and the OTLP collector. An invalid filter is
  refused when the configuration is read.
- `telemetry::Telemetry` is the one provider of a process. One meter
  provider is read by Prometheus (feature `prometheus`) and pushed over
  OTLP (feature `otlp`). One tracer provider exports the product's own
  spans and never an event. Neither reads a client's `traceparent`, so
  every request starts a trace of the server's own.
- `panic::install_hook` writes one fixed line per panic, with the thread
  and the source location and never the message.
- `request_log` (feature `axum`) writes one line and opens one span per
  request: the method, the route template, the status, the latency and the
  server's request id. It never carries a body, the raw path, a header
  value, an unlisted query value or the client's request id.

## Use

```toml
[dependencies]
ferrohealth-telemetry = { version = "=0.1.0", features = ["otlp", "prometheus", "axum"] }
```

Pin the version exactly, as every FerroHEALTH product does. A console leaves
`otlp` and `prometheus` off.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
