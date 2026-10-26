// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The requests one caller may send.
//!
//! The caller is known only once the product authenticated the request, so
//! the product puts a [`CallerKey`] in the request's extensions and mounts
//! [`limit`] inside its authentication. Each caller gets `burst` requests at
//! once and `requests_per_second` after that (the generic cell rate
//! algorithm); a request over it is answered `429 Too Many Requests` with
//! `Retry-After` (RFC 6585 §4, RFC 9110 §10.2.3). A request without a key is
//! not limited. No specification governs the rest: our own design.
//!
//! ```
//! use axum::Router;
//! use axum::routing::get;
//! use ferrohealth_server::caller_rate::{CallerLimit, limit};
//! use ferrohealth_server::section::CallerRate;
//!
//! let rate = CallerRate {
//!     requests_per_second: 10.try_into()?,
//!     burst: 20.try_into()?,
//! };
//! // The product's authentication layer, which inserts the `CallerKey`, is
//! // added after this one, so it runs first.
//! let routes: Router = Router::new()
//!     .route("/fhir/Patient", get(|| async { "[]" }))
//!     .route_layer(axum::middleware::from_fn_with_state(CallerLimit::new(rate), limit));
//! # Ok::<(), std::num::TryFromIntError>(())
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderValue, StatusCode, header};
use tokio::time::Instant;

use crate::section::CallerRate;

/// The caller a request is limited as, which the product's authentication
/// puts in the request's extensions.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallerKey(pub Arc<str>);

/// How many callers are kept before the ones whose burst refilled are
/// forgotten.
const PRUNE_AT: usize = 4096;

/// The limit shared by every request: each caller's next free instant.
#[derive(Debug, Clone)]
pub struct CallerLimit {
    interval: Duration,
    tolerance: Duration,
    callers: Arc<Mutex<BTreeMap<CallerKey, Instant>>>,
}

impl CallerLimit {
    /// The limit of `rate`.
    #[must_use]
    pub fn new(rate: CallerRate) -> Self {
        let interval = Duration::from_secs(1) / rate.requests_per_second.get();
        Self {
            interval,
            tolerance: interval.saturating_mul(rate.burst.get().saturating_sub(1)),
            callers: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Takes one request of `caller` at `now`: `Ok` when it may be served,
    /// and the wait until it may otherwise.
    fn take(&self, caller: &CallerKey, now: Instant) -> Result<(), Duration> {
        // NOTE: no specification governs this: our own design; a panic while
        // holding the map leaves instants only, which stay valid to read.
        let mut callers = self.callers.lock().unwrap_or_else(PoisonError::into_inner);
        let arrival = callers.get(caller).map_or(now, |&next| next.max(now));
        let ahead = arrival.saturating_duration_since(now);
        if ahead > self.tolerance {
            return Err(ahead.saturating_sub(self.tolerance));
        }
        if callers.len() >= PRUNE_AT {
            callers.retain(|_, next| *next > now);
        }
        callers.insert(caller.clone(), arrival + self.interval);
        Ok(())
    }
}

/// The middleware: serves a request within its caller's rate, and answers
/// `429` with `Retry-After` otherwise.
pub async fn limit(State(limit): State<CallerLimit>, request: Request, next: Next) -> Response {
    let Some(caller) = request.extensions().get::<CallerKey>() else {
        return next.run(request).await;
    };
    match limit.take(caller, Instant::now()) {
        Ok(()) => next.run(request).await,
        Err(wait) => {
            tracing::warn!("a request was refused: its caller sent more than its rate");
            let mut response = crate::refusal::refused(
                StatusCode::TOO_MANY_REQUESTS,
                "too many requests from this caller; retry later",
            );
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(crate::overload::retry_seconds(wait)),
            );
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use http::StatusCode;
    use tower::ServiceExt as _;

    use super::{CallerKey, CallerLimit, limit};
    use crate::section::CallerRate;

    fn app() -> Router {
        let rate = CallerRate {
            requests_per_second: 2.try_into().expect("non-zero"),
            burst: 3.try_into().expect("non-zero"),
        };
        Router::new()
            .route("/", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                CallerLimit::new(rate),
                limit,
            ))
            .layer(axum::middleware::from_fn(
                |mut request: axum::extract::Request, next: axum::middleware::Next| async move {
                    let caller = request
                        .headers()
                        .get("caller")
                        .and_then(|v| v.to_str().ok())
                        .map(Arc::<str>::from);
                    if let Some(caller) = caller {
                        request.extensions_mut().insert(CallerKey(caller));
                    }
                    next.run(request).await
                },
            ))
    }

    async fn call(app: &Router, caller: Option<&str>) -> (StatusCode, Option<String>) {
        let mut request = http::Request::builder().uri("/");
        if let Some(caller) = caller {
            request = request.header("caller", caller);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).expect("the request builds"))
            .await
            .expect("served");
        let retry = response
            .headers()
            .get(http::header::RETRY_AFTER)
            .map(|v| v.to_str().expect("ASCII").to_owned());
        (response.status(), retry)
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_is_served_then_refused_until_the_wait_passes() {
        let app = app();
        for _ in 0..3 {
            assert_eq!(StatusCode::OK, call(&app, Some("a")).await.0);
        }
        assert_eq!(
            (StatusCode::TOO_MANY_REQUESTS, Some("1".to_owned())),
            call(&app, Some("a")).await
        );
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(
            StatusCode::OK,
            call(&app, Some("a")).await.0,
            "one request refilled"
        );
        assert_eq!(StatusCode::TOO_MANY_REQUESTS, call(&app, Some("a")).await.0);
    }

    #[tokio::test(start_paused = true)]
    async fn two_callers_are_limited_apart_and_no_key_is_not_limited() {
        let app = app();
        for _ in 0..3 {
            assert_eq!(StatusCode::OK, call(&app, Some("a")).await.0);
        }
        assert_eq!(StatusCode::TOO_MANY_REQUESTS, call(&app, Some("a")).await.0);
        assert_eq!(
            StatusCode::OK,
            call(&app, Some("b")).await.0,
            "another caller has its own burst"
        );
        for _ in 0..10 {
            assert_eq!(StatusCode::OK, call(&app, None).await.0);
        }
    }
}
