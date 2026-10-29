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
  request log may carry, the deployment environment, and the OTLP
  collector. An invalid filter is refused when the configuration is read.
- `telemetry::Telemetry` is the one provider of a process. One meter
  provider is read by Prometheus (feature `prometheus`) and pushed over
  OTLP (feature `otlp`). One tracer provider exports the product's own
  spans and never an event. Neither reads a client's `traceparent`, so
  every request starts a trace of the server's own.
- `telemetry::Console` decides colour from the format, the terminal and
  `NO_COLOR`, and carries the directives no filter can drop. `install`
  returns the `FilterHandle` that replaces the filter at run time.
- `panic::install_hook` writes one fixed line per panic, with the thread,
  the source location and the server's id of the request being served
  when the request log wraps it, and never the message.
- `request_log` (feature `axum`) writes one line and opens one span per
  request: the method, the route template, the status, the latency and the
  server's request id. It never carries a body, the raw path, a header
  value, an unlisted query value, a listed value of the wrong `ValueShape`
  (such as digits only) or the client's request id. It records
  `http.server.request.duration` and `http.server.active_requests` on the
  one meter provider, writes the fields a handler adds with `LineFields`,
  and asks a `RouteNamer` for the route of a request no route matched.
- `telemetry::prometheus_route` (features `axum` and `prometheus`) serves
  the metrics in the Prometheus text format.

## Use

```toml
[dependencies]
ferrohealth-telemetry = { version = "=0.2.3", features = ["otlp", "prometheus", "axum"] }
```

Pin the version exactly, as every FerroHEALTH product does. A console leaves
`otlp` and `prometheus` off.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
