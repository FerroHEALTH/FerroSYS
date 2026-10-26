// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The health routes every product serves.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use ferrohealth_health::document::{Readiness, State as HealthState, Summary};
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use http::StatusCode;

use crate::build_info::BuildInfo;

/// The liveness route.
pub const LIVENESS: &str = "/health/liveness";

/// The readiness route.
pub const READINESS: &str = "/health/readiness";

/// The summary route.
pub const SUMMARY: &str = "/health";

/// The build facts route, served when the product supplies them.
pub const INFO: &str = "/health/info";

/// What `GET /health/info` reads: the build facts and the product's own.
type Facts = dyn Fn() -> serde_json::Map<String, serde_json::Value> + Send + Sync;

/// What the routes read: the indicators and the lifecycle.
#[derive(Clone)]
pub struct Health {
    registry: Registry,
    lifecycle: Lifecycle,
    info: Option<(BuildInfo, Arc<Facts>)>,
}

impl std::fmt::Debug for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Health")
            .field("registry", &self.registry)
            .field("lifecycle", &self.lifecycle)
            .field("info", &self.info.as_ref().map(|(build, _facts)| build))
            .finish()
    }
}

impl Health {
    /// The routes over `registry` in `lifecycle`'s phase.
    #[must_use]
    pub const fn new(registry: Registry, lifecycle: Lifecycle) -> Self {
        Self {
            registry,
            lifecycle,
            info: None,
        }
    }

    /// Serves `GET /health/info` with `build` and the facts `facts` returns
    /// on each request, such as the pins and the lanes of the product.
    ///
    /// A fact named `product`, `version` or `build` is replaced by the build
    /// facts of the same name.
    #[must_use]
    pub fn with_info(
        mut self,
        build: BuildInfo,
        facts: impl Fn() -> serde_json::Map<String, serde_json::Value> + Send + Sync + 'static,
    ) -> Self {
        self.info = Some((build, Arc::new(facts)));
        self
    }

    /// The lifecycle the routes read.
    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}

/// The three routes, and `/health/info` when the product supplied it,
/// relative to the base path.
///
/// - `GET /health/liveness` answers `200` while the process serves and
///   checks nothing.
/// - `GET /health/readiness` answers `200` with the readiness document while
///   the process is ready, and `503` with it otherwise. The route is open to
///   anyone who reaches the port, so the document names each indicator and
///   its state and never its detail, which can name a dependency or a count;
///   a product serves the detail behind its own authentication.
/// - `GET /health` answers the same status with the state alone, for a
///   caller that may not see the detail.
/// - `GET /health/info` answers `200` with `product`, `version`, the `build`
///   facts and the product's own facts.
pub fn routes(health: Health) -> Router {
    let routes = Router::new()
        .route(LIVENESS, get(liveness))
        .route(READINESS, get(readiness))
        .route(SUMMARY, get(summary));
    let routes = if health.info.is_some() {
        routes.route(INFO, get(info))
    } else {
        routes
    };
    routes.with_state(health)
}

async fn info(State(health): State<Health>) -> Response {
    let Some((build, facts)) = health.info else {
        return StatusCode::NOT_FOUND.into_response();
    };
    axum::Json(info_document(&build, facts())).into_response()
}

/// The `/health/info` document of `build` over the product's `facts`.
fn info_document(
    build: &BuildInfo,
    mut facts: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut facts_of_build = serde_json::Map::new();
    for (key, value) in [
        ("commit", build.commit()),
        ("commit_short", build.short_commit()),
        ("built_at", build.built_at()),
        ("rustc", build.rustc()),
    ] {
        if let Some(value) = value {
            facts_of_build.insert(key.to_owned(), value.into());
        }
    }
    facts.insert("product".to_owned(), build.name().into());
    facts.insert("version".to_owned(), build.version().into());
    facts.insert("build".to_owned(), facts_of_build.into());
    facts
}

async fn liveness() -> Response {
    axum::Json(Summary {
        state: HealthState::Up,
    })
    .into_response()
}

async fn readiness(State(health): State<Health>) -> Response {
    let document = health.registry.readiness(&health.lifecycle).await;
    (status(&document), axum::Json(document.without_detail())).into_response()
}

async fn summary(State(health): State<Health>) -> Response {
    let document = health.registry.readiness(&health.lifecycle).await;
    (status(&document), axum::Json(document.summary())).into_response()
}

/// `200` while ready, `503` otherwise.
fn status(document: &Readiness) -> StatusCode {
    if document.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use ferrohealth_health::lifecycle::Lifecycle;
    use ferrohealth_health::registry::Registry;
    use http::StatusCode;
    use tower::ServiceExt as _;

    use super::{Health, routes};
    use crate::build_info::BuildInfo;

    async fn info(health: Health) -> (StatusCode, serde_json::Value) {
        let response = routes(health)
            .oneshot(
                http::Request::builder()
                    .uri("/health/info")
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("served");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("the body");
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    #[tokio::test]
    async fn the_info_route_answers_the_build_and_the_product_facts() {
        let build = BuildInfo::new("ferrosys", "1.2.3")
            .with_commit("0123456789abcdef0123")
            .with_rustc("1.99.1");
        let health =
            Health::new(Registry::default(), Lifecycle::default()).with_info(build, || {
                let mut facts = serde_json::Map::new();
                facts.insert("pins".to_owned(), serde_json::json!({"fhir": "4.0.1"}));
                facts.insert(
                    "version".to_owned(),
                    "a product fact may not shadow it".into(),
                );
                facts
            });
        let (status, document) = info(health).await;
        assert_eq!(StatusCode::OK, status);
        assert_eq!(
            serde_json::json!({
                "product": "ferrosys",
                "version": "1.2.3",
                "build": {"commit": "0123456789abcdef0123", "commit_short": "0123456", "rustc": "1.99.1"},
                "pins": {"fhir": "4.0.1"},
            }),
            document
        );
    }

    #[tokio::test]
    async fn without_info_the_route_is_not_served() {
        let (status, _document) =
            info(Health::new(Registry::default(), Lifecycle::default())).await;
        assert_eq!(StatusCode::NOT_FOUND, status);
    }

    #[derive(Debug)]
    struct Spool;

    impl ferrohealth_health::registry::HealthIndicator for Spool {
        fn name(&self) -> &'static str {
            "spool"
        }

        fn check(&self) -> ferrohealth_health::registry::Check<'_> {
            Box::pin(std::future::ready(
                ferrohealth_health::registry::Outcome::degraded("4096 records behind at db-7"),
            ))
        }
    }

    #[tokio::test]
    async fn the_public_readiness_names_each_indicator_and_never_its_detail() {
        let lifecycle = Lifecycle::default();
        lifecycle.booted();
        let health = Health::new(Registry::new(vec![std::sync::Arc::new(Spool)]), lifecycle);
        let response = routes(health)
            .oneshot(
                http::Request::builder()
                    .uri("/health/readiness")
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("served");
        assert_eq!(StatusCode::OK, response.status(), "degraded stays ready");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("the body");
        assert_eq!(
            r#"{"state":"degraded","phase":"serving","indicators":[{"name":"spool","state":"degraded"}]}"#,
            String::from_utf8_lossy(&body)
        );
    }
}
