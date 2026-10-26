// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The bound on the requests served at once.
//!
//! Past the bound a request is answered `503 Service Unavailable` with
//! `Retry-After` at once (RFC 9110 §15.6.4, §10.2.3) and reaches nothing
//! behind it. The stack mounts the bound on the product's routes alone, so
//! the health family is never refused and a busy server is not restarted by
//! its own probe.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use http::{HeaderValue, StatusCode, header};
use tokio::sync::Semaphore;

/// The admission: the slots and the wait a refused request is told.
#[derive(Debug, Clone)]
pub struct Admission {
    slots: Arc<Semaphore>,
    retry_after: Duration,
}

impl Admission {
    /// Admits at most `limit` requests at once, telling a refused one to
    /// wait `retry_after`.
    #[must_use]
    pub fn new(limit: NonZeroUsize, retry_after: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(limit.get().min(Semaphore::MAX_PERMITS))),
            retry_after,
        }
    }
}

/// The middleware: serves the request in a slot, or refuses it.
pub async fn admit(State(admission): State<Admission>, request: Request, next: Next) -> Response {
    let Ok(_slot) = Arc::clone(&admission.slots).try_acquire_owned() else {
        tracing::warn!(
            "a request was refused: the server serves as many requests as it takes at once"
        );
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            "the server is busy; retry later",
        )
            .into_response();
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(retry_seconds(admission.retry_after)),
        );
        return response;
    };
    next.run(request).await
}

/// The whole seconds of `wait`, rounded up and at least one.
pub(crate) fn retry_seconds(wait: Duration) -> u64 {
    wait.as_secs()
        .saturating_add(u64::from(wait.subsec_nanos() > 0))
        .max(1)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::retry_seconds;

    #[test]
    fn the_wait_rounds_up_to_whole_seconds_and_is_never_zero() {
        assert_eq!(1, retry_seconds(Duration::ZERO));
        assert_eq!(1, retry_seconds(Duration::from_millis(200)));
        assert_eq!(2, retry_seconds(Duration::from_millis(1200)));
        assert_eq!(3, retry_seconds(Duration::from_secs(3)));
    }
}
