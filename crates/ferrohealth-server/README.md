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
  - the refusals the stack writes itself (`408`, `413`, `429`, `503`) in
    the body the product supplies, such as a FHIR `OperationOutcome`, which
    may answer a status of its own with `refusal::keep_status`;
  - the overload bound: past `max_concurrent_requests` a request is answered
    `503` with `Retry-After`, and the health routes are never refused;
  - the request log of `ferrohealth-telemetry`, one line for every request,
    a refusal and a panic included.
- `stack::Stack` builds the same stack and also mounts routes beside the
  base path (`beside`) and the product's own answer to a request no route
  matched (`fallback`), each inside every layer above.
- `health::routes` serves `/health`, `/health/liveness` and
  `/health/readiness` over `ferrohealth-health`. Readiness names each
  indicator and its state and never its detail, since anyone who reaches
  the port reads it.
- `serve::serve` runs until `SIGTERM` or `SIGINT`. It then withdraws
  readiness, waits the drain delay, and gives the requests in flight a
  bounded time to finish. A client gets `header_read_timeout_ms` to send a
  request head, counted from the head's first byte, so a connection that
  never finishes one is closed, and a kept-alive connection is closed
  after `idle_timeout_ms` without a request.
- `healthcheck::check` is the container probe: readiness over loopback,
  exit `0` only on `200`. `healthcheck::Get` makes the same exchange for a
  product that reads more from its own listener, over plain HTTP or the
  pinned TLS, and answers the status and the body, read up to a bound and
  within a timeout.
- `cli::Common` holds the `serve`, `config check` and `healthcheck`
  subcommands every binary flattens into its own, and `cli::ConfigArgs`
  the `--config` and `--set` arguments.
- `section::ServerSection` is the `[server]` section, with the listener's
  TLS (mutual TLS, reload on `SIGHUP`, TLS 1.3 unless `min_version`
  lowers it to 1.2), the trusted proxies, the caller
  rate and the compression.
- `serve::run_with_tls` and `serve::Listener::with_reloadable_tls` serve
  a `tls::ReloadableTls` the product keeps and reloads from its own reload
  path; `tls::TlsKeys` (`tls_keys!("metrics.tls")`) names the table a
  listener's TLS faults come from.
- `serve::run_all` serves several listeners under one signal,
  `client_address` names a request's client, `caller_rate` limits one
  caller, `health::Health::with_info` serves `/health/info`, and
  `tls_client` reads the TLS material of outbound calls (feature `reqwest`
  sets it on a reqwest client).
- `problem` writes RFC 9457 problem details (`application/problem+json`)
  for a product's error bodies outside FHIR: a `Problem` with the RFC's
  members and extension members, the product's types defined once in a
  `ProblemTypes` registry under a URI it documents, and
  `problem::refusal_body` and `problem::panic_body`, which a product sets
  in its `StackPolicy` to answer the stack's refusals and panics as
  problems. The body is `application/problem+json` whatever the client
  asked for; it is JSON, so a client that accepts `application/json`
  reads it.
- `console` (feature `console`) adds an operator console's
  Content-Security-Policy and the security headers beside it: a nonce
  policy for a hydrated console (`ConsoleHeaders::new`), or
  `script-src 'self'; style-src 'self'` with no nonce for a server-rendered
  one (`ConsoleHeaders::server_rendered`), and `same_origin`, which refuses
  a state-changing request another site sent.

## Use

```toml
[dependencies]
ferrohealth-server = "=0.4.1"
```

Install `ferrohealth_telemetry::panic::install_hook` at start, so Rust's own
hook prints no panic message. Pin the version exactly, as every FerroHEALTH
product does.

## Licence

Business Source License 1.1, with Apache License 2.0 as the Change License
four years after each version; see the repository's `LICENSE`.
