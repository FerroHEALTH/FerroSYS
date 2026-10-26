// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The health routes every product serves.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use ferrohealth_health::document::{Readiness, State as HealthState, Summary};
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use http::StatusCode;

/// The liveness route.
pub const LIVENESS: &str = "/health/liveness";

/// The readiness route.
pub const READINESS: &str = "/health/readiness";

/// The summary route.
pub const SUMMARY: &str = "/health";

/// What the routes read: the indicators and the lifecycle.
#[derive(Debug, Clone)]
pub struct Health {
    registry: Registry,
    lifecycle: Lifecycle,
}

impl Health {
    /// The routes over `registry` in `lifecycle`'s phase.
    #[must_use]
    pub const fn new(registry: Registry, lifecycle: Lifecycle) -> Self {
        Self {
            registry,
            lifecycle,
        }
    }

    /// The lifecycle the routes read.
    #[must_use]
    pub const fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }
}

/// The three routes, relative to the base path.
///
/// - `GET /health/liveness` answers `200` while the process serves and
///   checks nothing.
/// - `GET /health/readiness` answers `200` with the readiness document while
///   the process is ready, and `503` with it otherwise.
/// - `GET /health` answers the same status with the state alone, for a
///   caller that may not see the detail.
pub fn routes(health: Health) -> Router {
    Router::new()
        .route(LIVENESS, get(liveness))
        .route(READINESS, get(readiness))
        .route(SUMMARY, get(summary))
        .with_state(health)
}

async fn liveness() -> Response {
    axum::Json(Summary {
        state: HealthState::Up,
    })
    .into_response()
}

async fn readiness(State(health): State<Health>) -> Response {
    let document = health.registry.readiness(&health.lifecycle).await;
    (status(&document), axum::Json(document)).into_response()
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
