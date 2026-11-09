// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one stack every server runs around its routes.

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::handler::Handler;
use ferrohealth_telemetry::request_log::RequestLog;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::{NotForContentType, Predicate as _, SizeAbove};
use tower_http::limit::RequestBodyLimitLayer;

use crate::api_headers;
use crate::client_address::Forwarding;
use crate::edge::{self, EdgeLayer, IdentifyLayer};
use crate::health::{self, Health};
use crate::overload::{self, Admission};
use crate::panic::{self, PanicBody};
use crate::refusal::{self, RefusalBody};
use crate::section::ServerSection;

/// What a product decides inside the stack: the body of a panicked request,
/// the body of a refusal and the request log with its recorders.
#[derive(Clone)]
pub struct StackPolicy {
    /// The body of a panicked request.
    pub panic_body: PanicBody,
    /// The body of a refusal the stack writes: `408`, `413`, `429`, `503`.
    pub refusal_body: RefusalBody,
    /// The request log.
    pub request_log: RequestLog,
}

impl std::fmt::Debug for StackPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StackPolicy")
            .field("request_log", &self.request_log)
            .finish_non_exhaustive()
    }
}

impl Default for StackPolicy {
    fn default() -> Self {
        Self {
            panic_body: panic::default_body(),
            refusal_body: Arc::clone(&refusal::DEFAULT_BODY),
            request_log: RequestLog::default(),
        }
    }
}

/// Wraps `product` and the health routes in the stack, under the base path.
///
/// From the outside in: the API security headers ([`crate::api_headers`]),
/// the request id, the request log, the client address, the refusal
/// rendering, the panic rendering, the panic catch, the request timeout
/// (`408`), the body limit (`413`, and the same ceiling for every
/// extractor), the response compression when it is on, then the routes,
/// the product's behind the overload bound when one is configured.
/// Every request leaves one request line, a refusal and a panic included.
///
/// It is [`Stack::new`] with nothing beside the base path, and a request no
/// route matched takes the product's fallback with the product's layers
/// (axum's empty `404` unless the product changes it); [`Stack`] adds both.
pub fn app(
    product: Router,
    health: Health,
    section: &ServerSection,
    policy: StackPolicy,
) -> Router {
    Stack::new(product, health, section, policy).into_router()
}

/// The stack around a product, with the routes it serves beside its base
/// path and the fallback of a request no route matched.
///
/// [`Stack::into_router`] mounts the product and the health routes under
/// the base path, as [`app`] does, the routes of [`Stack::beside`] at the
/// root next to them, and the handler of [`Stack::fallback`] for every
/// request no route matched. Every layer of [`app`] covers all three, and
/// the overload bound covers the product's routes and the routes beside.
///
/// # Examples
///
/// ```
/// use axum::Router;
/// use axum::routing::get;
/// use ferrohealth_health::lifecycle::Lifecycle;
/// use ferrohealth_health::registry::Registry;
/// use ferrohealth_server::health::Health;
/// use ferrohealth_server::section::{BasePath, ServerSection};
/// use ferrohealth_server::stack::{Stack, StackPolicy};
/// use http::StatusCode;
///
/// let section = ServerSection {
///     base_path: BasePath::new("/ehr")?,
///     ..ServerSection::default()
/// };
/// let health = Health::new(Registry::default(), Lifecycle::default());
/// // `/ehr/v1/ehr` and `/ehr/health` under the base path, `/fhir/metadata`
/// // beside it, and the product's own `404` for every other path.
/// let product = Router::new().route("/v1/ehr", get(|| async { "ehr" }));
/// let fhir = Router::new().route("/fhir/metadata", get(|| async { "metadata" }));
/// let app: Router = Stack::new(product, health, &section, StackPolicy::default())
///     .beside(fhir)
///     .fallback(|| async { (StatusCode::NOT_FOUND, "no such resource") })
///     .into_router();
/// # let _ = app;
/// # Ok::<(), ferrohealth_server::section::BasePathError>(())
/// ```
#[derive(Debug)]
#[must_use]
pub struct Stack {
    product: Router,
    health: Health,
    section: ServerSection,
    policy: StackPolicy,
    beside: Router,
    fallback: Option<Router>,
}

impl Stack {
    /// The stack around `product` and `health` under `section`, with
    /// `policy`: what [`app`] builds.
    pub fn new(
        product: Router,
        health: Health,
        section: &ServerSection,
        policy: StackPolicy,
    ) -> Self {
        Self {
            product,
            health,
            section: section.clone(),
            policy,
            beside: Router::new(),
            fallback: None,
        }
    }

    /// Mounts `routes` at the root, beside the base path, with every layer
    /// of the stack and the overload bound around them.
    ///
    /// A path of `routes` is served as written, whatever the base path; with
    /// an empty base path they sit next to the product's routes. Called more
    /// than once, the routes add up.
    ///
    /// Without [`Stack::fallback`], a path no route matched takes the
    /// fallback `routes` declare when they declare one, and the product's
    /// otherwise, with every layer the product applied. Under an empty base
    /// path that is the product's own or default fallback; under a non-empty
    /// one only a fallback the product declares, for the paths under the
    /// base path (axum `Router::nest`, "Fallbacks"), and axum's empty `404`
    /// for the rest.
    pub fn beside(mut self, routes: Router) -> Self {
        self.beside = self.beside.merge(routes);
        self
    }

    /// Answers every request no route matched with `handler`, inside every
    /// layer of the stack and outside the overload bound.
    ///
    /// It answers a path outside the base path and every route beside it,
    /// and a path under the base path that no route matched, unless the
    /// product's router has a fallback of its own (axum `Router::nest`,
    /// "Fallbacks"). A second call replaces the first.
    pub fn fallback<H, T>(mut self, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.fallback = Some(Router::new().fallback(handler));
        self
    }

    /// Builds the router the listener serves.
    ///
    /// # Panics
    ///
    /// When axum refuses the routes it merges: a route beside the base path
    /// that is also a route under it, or, with [`Stack::fallback`] set, a
    /// router beside or a product under an empty base path that carries a
    /// fallback of its own (axum `Router::merge`, "Panics").
    pub fn into_router(self) -> Router {
        let Self {
            product,
            health,
            section,
            policy,
            beside,
            fallback,
        } = self;
        let admission = section
            .max_concurrent_requests
            .map(|limit| Admission::new(limit, section.retry_after.get()));
        let product = match &admission {
            Some(admission) => product.route_layer(axum::middleware::from_fn_with_state(
                admission.clone(),
                overload::admit,
            )),
            None => product,
        };
        let routes = health::routes(health).merge(product);
        let routes = if section.base_path.as_str().is_empty() {
            routes
        } else {
            Router::new().nest(section.base_path.as_str(), routes)
        };
        let beside = match admission {
            Some(admission) if beside.has_routes() => beside.route_layer(
                axum::middleware::from_fn_with_state(admission, overload::admit),
            ),
            _ => beside,
        };
        // NOTE: axum 0.8 `Router::merge` keeps the fallback of `other` when neither router
        // declares one, so `routes` merges last and keeps the product's layered fallback.
        let routes = beside.merge(routes);
        let routes = match fallback {
            Some(fallback) => routes.merge(fallback),
            None => routes,
        };
        wrap(routes, &section, policy)
    }
}

/// Wraps `routes` in every layer of the stack.
fn wrap(routes: Router, section: &ServerSection, policy: StackPolicy) -> Router {
    // NOTE: axum `Router::layer` wraps every route and the fallback, so the matched
    // route is still known and a request no route matched is logged too.
    let edge = EdgeLayer::new(edge::Settings {
        forwarding: Forwarding::from_section(section),
        panic_body: policy.panic_body,
        refusal_body: policy.refusal_body,
        timeout: section.request_timeout.get(),
        body_limit: section.body_limit_bytes(),
    });
    let transport = api_headers::Transport {
        tls: section.tls.is_some(),
    };
    let outer = ServiceBuilder::new()
        .layer(axum::middleware::from_fn_with_state(
            transport,
            api_headers::set,
        ))
        .layer(IdentifyLayer)
        .layer(policy.request_log)
        .layer(edge)
        .layer(RequestBodyLimitLayer::new(section.body_limit_bytes()))
        .layer(DefaultBodyLimit::max(section.body_limit_bytes()));
    if section.compression {
        routes.layer(
            outer.layer(
                CompressionLayer::new().gzip(true).br(true).compress_when(
                    SizeAbove::new(section.compression_min_size.bytes())
                        .and(NotForContentType::GRPC)
                        .and(NotForContentType::IMAGES)
                        .and(NotForContentType::SSE),
                ),
            ),
        )
    } else {
        routes.layer(outer)
    }
}
