<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# FerroSYS architecture

This is the design of record for FerroSYS and, while no code exists, the
plan. It comes from a survey of the five FerroHEALTH servers on 2026-10-26
and the owner's rulings on it the same day. Each section says what is
decided; a change to a decision is made here, in the same pull request as
the change it explains.

## 1. What FerroSYS is

FerroSYS is a repository of shared Rust crates, the same kind as FerroAUTH
and FerroAUDIT. The crates carry the system plumbing every FerroHEALTH
server (FerroEHR, FerroBRIDGE, FerroFED, FerroPIX, FerroTERM) and every
operator console otherwise writes for itself. They run inside each product
and take no call of their own.

FerroSYS distributes no configuration, deploys nothing and runs no service.
No central service is planned.

The survey found the same plumbing written five times and drifting: one
console format rule copied into every server, one health registry trait
copied into four, one configuration loader copied into three and every
console, a byte-identical `usage_report/state.rs` in three servers, a
byte-identical console `command.rs` and `healthcheck.rs` in four consoles,
the FerroPULSE envelope schema vendored five times, and the URL userinfo
mask written at least five times across the products, FerroAUTH and
FerroAUDIT. The family rule is that every shared concern lives in one
crate; FerroSYS is that crate set for system plumbing.

### What stays elsewhere

| Concern | Home |
|---|---|
| Authentication, the policy decision, and the console OIDC sign-in and session store | FerroAUTH (`ferrohealth-smart`, `ferrohealth-authz`) |
| The audit trail and the EHDS access record | FerroAUDIT (`ihe-atna`, `ehds-logging`) |
| Console UI components | FerroUI (`ferrohealth-ui`) |
| FerroEHR's management surface (`/management/*`, the `pprof` flamegraph, the env and logger routes) | FerroEHR, for now |
| A product's domain (FHIR, OMOP, openEHR, federation, identity, terminology) | the product |

## 2. The crates

Six library crates, split by what a binary takes on its own, the way
`ferrohealth-smart` and `ferrohealth-authz` split. Every name is
`ferrohealth-*`, like FerroAUTH and FerroUI; the name `ferrosys` on
crates.io belongs to an unrelated project.

| Crate | Holds | Why it stands alone | Taken by |
|---|---|---|---|
| `ferrohealth-secret` | `Secret`, `SecretUrl`, `RedactedUrl`, `Redacted<T>`, the `<key>` / `<key>_file` rule | the base every other crate needs, FerroAUTH and FerroAUDIT included, so it stays small: no TOML, no HTTP, no OpenTelemetry | every server and console, FerroAUTH, FerroAUDIT, the other five crates |
| `ferrohealth-config` | the TOML file, the `PREFIX__A__B` environment overrides, the strict checks, provenance, the `config check` rendering | consoles and job binaries take it without a server | every server and console |
| `ferrohealth-telemetry` | the console subscriber, the request log, OTLP traces and metrics, the Prometheus reader; `otlp`, `prometheus` and `axum` features | every binary wants the console; a console leaves the exporters off | every server and console |
| `ferrohealth-health` | the readiness document (serde only, builds for `wasm32`); the indicator trait, registry and lifecycle behind the `registry` feature | the document crosses into a console's browser half and each operator-api crate | servers (registry), consoles and operator-api crates (document) |
| `ferrohealth-server` | the HTTP middleware stack, the shutdown drain, the health routes, the healthcheck client, the CLI skeleton, the build info and banner; the console security headers behind `console` | one unit every server and console takes whole | every server and console |
| `ferrohealth-usage-report` | the FerroPULSE envelope v2, the instance id under `state_dir`, the latency window, the `start` and `daily` reporter | its own wire contract and off switch; only servers send it | the five servers |

### Dependency direction

```
ferrohealth-secret  <-  ferrohealth-config  <-  ferrohealth-server
        ^                     ^                       |
        |                     |                       v
 ferrohealth-smart     ferrohealth-telemetry   ferrohealth-health
 ihe-atna                     ^
 ehds-logging          ferrohealth-usage-report
```

An arrow points at the dependency. `ferrohealth-server` depends on
`-config`, `-telemetry` and `-health`. No FerroSYS crate depends on FerroAUTH
or FerroAUDIT; they depend only on `ferrohealth-secret`, so there is no cycle.

## 3. Public API sketch

The shapes below are the plan, refined when each crate is built.

```rust
// ferrohealth-secret (deps: secrecy, serde, url, thiserror)
pub const REDACTED: &str = "***";
pub struct Secret(SecretString);          // Debug, Display and Serialize render REDACTED
pub struct SecretUrl(SecretString);       // renders with userinfo and credential query values masked
pub struct RedactedUrl<'a>(pub &'a str);  // the rendering of any URL, moved from ihe-atna
pub struct Redacted<T>(pub T);            // Debug names the type and prints REDACTED
pub fn read_secret(key: &str, value: Option<Secret>, file: Option<&Path>) -> Result<Option<Secret>, SecretError>;

// ferrohealth-config (deps: toml, serde, ferrohealth-secret)
pub struct Loader;   // Loader::new("FERROSYS")?.alias("RUST_LOG", "telemetry.filter").reserve("FERROSYS_STATE_DIR")
impl Loader {
    pub fn load<T: DeserializeOwned>(&self, path: Option<&Path>) -> Result<Loaded<T>, ConfigError>;
    pub fn load_sources<T: DeserializeOwned>(&self, text: Option<&str>, path: Option<&Path>, environment: &BTreeMap<String, String>) -> Result<Loaded<T>, ConfigError>;
}
pub struct Loaded<T>;  // settings(), provenance(), into_parts()
pub fn check_report<T: Serialize>(loaded: &Loaded<T>) -> Result<String, ConfigError>;

// ferrohealth-telemetry
pub struct TelemetrySection { pub format: Format, pub filter: String, pub otlp: Option<OtlpSection>, pub logged_query_parameters: Vec<String> }
pub enum Format { Auto, Json, Pretty }
pub struct Telemetry;  // Telemetry::new(&section, Service::new("ferropix", VERSION))?
impl Telemetry { pub fn install(&self, section: &TelemetrySection, stdout_is_terminal: bool) -> Result<(), InitError>; pub fn meter(&self, scope: &'static str) -> Meter; pub fn shutdown(&self) -> Result<(), InitError>; }
pub fn panic::install_hook();
pub async fn request_log::middleware(State(log): State<RequestLog>, request: Request, next: Next) -> Response;  // axum feature

// ferrohealth-health
pub struct Readiness { pub state: State, pub phase: Phase, pub indicators: Vec<IndicatorState> }
pub enum State { Up, Degraded, Down }   // degraded still takes requests; down does not
pub enum Phase { Booting, Serving, Draining }
pub trait HealthIndicator: Debug + Send + Sync { fn name(&self) -> &'static str; fn check(&self) -> Check<'_>; }   // registry feature
impl Registry { pub async fn readiness(&self, lifecycle: &Lifecycle) -> Readiness; }

// ferrohealth-server
pub fn stack::app(product: Router, health: Health, section: &ServerSection, policy: StackPolicy) -> Router;
pub fn health::routes(health: Health) -> Router;
pub async fn serve::serve(listener: TcpListener, app: Router, lifecycle: Lifecycle, signal: F, delay: Duration, bound: Duration) -> io::Result<Drained>;
pub async fn healthcheck::check(address: SocketAddr, base_path: &str, timeout: Duration) -> Outcome;
#[derive(clap::Subcommand)] pub enum Common { Serve, Config { cmd: ConfigCmd }, Healthcheck }

// ferrohealth-usage-report
pub trait ProductSection: Send + Sync { type Status: Serialize; const PRODUCT: Product; fn status(&self) -> impl Future<Output = Self::Status> + Send; }
pub struct Reporter<P: ProductSection>;   // start(), send_start(), send_daily(), spawn()
pub struct Window;   // Window::new(grouping), record(method, route, status, latency), count(key)
```

## 4. Policies

Where the products disagree today, the stricter option is the family
behaviour. No specification governs these choices unless one is named: they
are our own design.

- **Request id.** The server mints its own id for every request, logs it and
  sends it upstream. A client's `X-Request-Id` is honoured, echoed on the
  response, and never logged; the log line says only whether the client sent
  one, because the text is free and may name a patient.
- **Trace context.** A client's `traceparent` and `tracestate` are ignored
  (W3C Trace Context); every request starts a trace of the server's own.
- **Log filter.** An invalid filter, from the file or from `RUST_LOG`, refuses
  the boot with the error; there is no fallback filter.
- **Panic messages.** A handler panic becomes a `500` and one fixed log line.
  The panic message reaches neither the body, the log nor stderr; the panic
  hook replaces Rust's default. A product supplies only the body shape (FHIR
  routes answer an `OperationOutcome`, per the FHIR RESTful API).
- **Health routes.** Every product serves `/health`, `/health/liveness` and
  `/health/readiness` under its base path. Liveness answers `200` while the
  process serves and checks nothing. Readiness answers `200` only while the
  process is serving and every indicator is up, with the readiness document;
  it answers `503` from the moment a stop is requested. `/health` answers the
  same status as readiness with the status alone. Every server and console
  has the `healthcheck` subcommand for a container `HEALTHCHECK`.
- **The request log** names the method, the matched route template, the
  status, the latency and the server's request id. It never carries a body,
  the raw path, a header value, or a query value outside a configured
  allow-list.
- **Redaction.** Every secret is a `Secret` or `SecretUrl`. Every type that
  holds clinical payload or a patient identifier implements `Debug` through
  `Redacted<T>` or by hand with the same rule; this is mandatory family-wide.
- **Configuration.** One loader: FerroBRIDGE's TOML file with
  `PREFIX__A__B` overrides placed as text and retried typed, every struct
  `deny_unknown_fields`, every secret with a `_file` sibling, and FerroEHR's
  strict checks (the reserved-prefix environment sweep and the did-you-mean
  suggestion). Every server and console has `config check`.
- **Metrics.** One OpenTelemetry meter provider in every product, read by
  Prometheus on `/metrics` and, when configured, pushed over OTLP.
- **Shutdown.** On `SIGTERM` or `SIGINT` readiness turns `503`, a configured
  delay passes, the listener closes, and the requests in flight finish
  within a configured bound; a connection still open then is dropped.

## 5. Where each crate starts

The starting code is the most complete product implementation.

| Crate | Starts from | Adds from |
|---|---|---|
| `ferrohealth-secret` | FerroFED `ferrofed-registry/src/secret.rs` | `ihe-atna::redact::RedactedUrl`, the `ferrohealth-smart` exactly-one `_file` rule, FerroFED's `read_secret` |
| `ferrohealth-config` | FerroBRIDGE's `config/` loader, overrides and provenance | FerroEHR's `config/strict.rs` |
| `ferrohealth-telemetry` | FerroFED's `telemetry.rs` and `metrics/` | FerroBRIDGE's request log, FerroTERM's line-safe writer |
| `ferrohealth-health` | FerroFED's `health/` with its lifecycle | FerroEHR's per-check timeout and `degraded` state |
| `ferrohealth-server` | FerroFED's request id, panic hook, overload bound, listener TLS, drain and healthcheck | FerroBRIDGE's build info, the console `command.rs`, `healthcheck.rs` and security headers |
| `ferrohealth-usage-report` | FerroBRIDGE's `usage_report/` | the FerroPULSE schema, vendored once here |

## 6. Shared pins

The versions the products run today; each is verified on crates.io when a
crate adds it. `secrecy` 0.10.3, `serde` 1.0.229, `serde_json` 1.0.151,
`thiserror` 2.0.21, `toml` 1.1, `url` 2.5.8, `tokio` 1.53, `tracing` 0.1.44,
`tracing-subscriber` 0.3.23, `opentelemetry`, `opentelemetry_sdk`,
`opentelemetry-otlp` and `opentelemetry-proto` 0.33, `tracing-opentelemetry`
0.34, `opentelemetry-prometheus` 0.33, `prometheus` 0.14, `axum` 0.8.9,
`tower` 0.5.3, `tower-http` 0.7.1, `hyper` 1.11, `hyper-util` 0.1.21,
`rustls` 0.23.46 with `aws_lc_rs`, `tokio-rustls` 0.26.6, `reqwest` 0.13.5,
`jiff` 0.2.37, `uuid` 1.26.1, `clap` 4.6.7. Toolchain 1.99.1, MSRV 1.98,
edition 2024, the family lint set.

## 7. Versioning and release

Each crate has its own version line, changelog and tag, `<crate>-vX.Y.Z`,
the scheme FerroAUTH uses since `ferrohealth-smart-v0.7.0`. Products pin
each crate exactly (`=X.Y.Z`). Every FerroSYS release rolls out to every
product at once, one pull request per product (lockstep).

The first step is reserving the six names on crates.io, which needs the
owner. No crate is built before its name is reserved.

## 8. Migration order

Smallest risk first, one pull request per product per step.

1. FerroSYS releases `ferrohealth-secret`; FerroAUDIT moves `RedactedUrl` to
   it and FerroAUTH's `OauthSection` mask and `_file` rule use it.
2. The four product consoles (FerroBRIDGE, FerroPIX, FerroTERM, FerroFED),
   whose `command.rs` and `healthcheck.rs` are identical today.
3. FerroPIX, the smallest server and already a copy of FerroBRIDGE and
   FerroFED.
4. FerroBRIDGE, which gains `healthcheck`, `config check`, OTLP and metrics.
5. FerroFED, the source of most of the code, so mostly mechanical.
6. FerroTERM, which moves from environment-only configuration to TOML with
   `FERROTERM__` overrides and gains the panic catch, timeout, body limit and
   bounded drain; an operator-visible change that needs a compatibility
   window.
7. FerroEHR last: the largest server, the `config` crate, `axum-server`, and
   the most operator surface to keep stable.

## 9. Standards

Vendored under `docs/specs/` with a `PROVENANCE.md` each when the crate that
needs them lands.

| Corpus | Used by |
|---|---|
| OpenTelemetry Protocol specification and `opentelemetry-proto` at the tag of the 0.33 crate line | `-telemetry` |
| OpenTelemetry semantic conventions v1.44.0, `docs/specs/otel-semconv/` (`service.*`, `http.route`, `http.request.method`, `http.response.status_code`, `deployment.environment.name`, `http.server.request.duration`) | `-telemetry` |
| W3C Trace Context | `-telemetry` (the policy in §4) |
| The Prometheus and OpenMetrics text exposition formats | `-telemetry` |
| RFC 9110 (status codes, `Retry-After`) | `-server` |
| RFC 3986 §3.2.1 (userinfo) | `-secret` |
| W3C Content Security Policy Level 3 | `-server` (`console`) |
| The FerroPULSE report envelope v2 schema | `-usage-report` |

The IETF `draft-inadarei-api-health-check` and RFC 9457 are not adopted: no
product uses either, and the readiness document is our own design.

## 10. Decision register

| Date | Decision | Source |
|---|---|---|
| 2026-10-26 | FerroSYS is a repository of shared crates; no configuration distribution, no deployment, no central service | owner |
| 2026-10-26 | Six crates named `ferrohealth-*`, per-crate tags, lockstep | owner |
| 2026-10-26 | The stricter option for each policy in §4 | owner |
| 2026-10-26 | FerroBRIDGE's loader with FerroEHR's strict checks; one OpenTelemetry provider in every product | owner |
| 2026-10-26 | `Redacted<T>` mandatory for clinical payloads | owner |
| 2026-10-26 | The console OIDC sign-in and session store go to FerroAUTH | owner |
| 2026-10-26 | FerroEHR's management surface stays in FerroEHR for now | owner |
| 2026-10-26 | The lockup is drawn in the parent's steel, as for every shared library | owner |
