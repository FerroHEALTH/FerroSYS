// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The bound on the requests served at once, with a short wait in front of
//! it.
//!
//! A request past the bound waits for a slot in a bounded queue, first come
//! first served, for at most the configured wait. When the queue is full,
//! or the wait expires, the request is answered `503 Service Unavailable`
//! with `Retry-After` (RFC 9110 §15.6.4, §10.2.3) and reaches nothing
//! behind it, so the requests in flight and the requests waiting never
//! exceed the two bounds. The wait rides out a server-side stall of a few
//! hundred milliseconds that would otherwise refuse requests the server
//! answers a moment later. The stack mounts the bound on the product's
//! routes alone, so the health family is never refused or queued and a busy
//! server is not restarted by its own probe. No specification governs the
//! queue or its sizes: our own design.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderValue, StatusCode, header};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The counter of the requests that waited for a slot.
pub const WAITS: &str = "ferrohealth.server.admission.waits";

/// The histogram of the time a request waited for a slot, in seconds, with
/// [`OUTCOME`] `admitted` or `expired`.
pub const WAIT_DURATION: &str = "ferrohealth.server.admission.wait.duration";

/// The counter of the requests refused past the bound, with [`REASON`]
/// `queue_full` or `wait_expired`.
pub const REFUSALS: &str = "ferrohealth.server.admission.refusals";

/// The attribute of [`WAIT_DURATION`] that says how a wait ended.
pub const OUTCOME: &str = "ferrohealth.server.admission.outcome";

/// The attribute of [`REFUSALS`] that says why a request was refused.
pub const REASON: &str = "ferrohealth.server.admission.reason";

/// The bucket boundaries of [`WAIT_DURATION`], in seconds, around the
/// default wait of 100 ms.
const WAIT_BUCKETS: [f64; 12] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 1.0, 2.5,
];

/// The admission: the slots, the queue in front of them, and the wait a
/// refused request is told.
#[derive(Debug, Clone)]
pub struct Admission {
    slots: Arc<Semaphore>,
    queue: Option<Queue>,
    retry_after: Duration,
    metrics: Option<Metrics>,
}

/// The places of the queue and how long a request may hold one.
#[derive(Debug, Clone)]
struct Queue {
    places: Arc<Semaphore>,
    wait: Duration,
}

/// The instruments the admission records in.
#[derive(Debug, Clone)]
struct Metrics {
    waits: Counter<u64>,
    waited: Histogram<f64>,
    refusals: Counter<u64>,
}

/// Why a request past the bound was refused.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    /// The queue had no place left, or there is no queue.
    QueueFull,
    /// No slot came free within the wait.
    WaitExpired,
}

impl Refusal {
    /// The value of [`REASON`].
    const fn reason(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::WaitExpired => "wait_expired",
        }
    }
}

impl Admission {
    /// Admits at most `limit` requests at once, telling a refused one to
    /// wait `retry_after`. A request past the bound is refused at once
    /// until [`Admission::with_queue`] gives it a place to wait.
    #[must_use]
    pub fn new(limit: NonZeroUsize, retry_after: Duration) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(limit.get().min(Semaphore::MAX_PERMITS))),
            queue: None,
            retry_after,
            metrics: None,
        }
    }

    /// Lets at most `places` requests past the bound wait for a slot, each
    /// for at most `wait`, first come first served. Zero places or a zero
    /// wait keep no queue.
    #[must_use]
    pub fn with_queue(mut self, places: usize, wait: Duration) -> Self {
        self.queue = (places > 0 && !wait.is_zero()).then(|| Queue {
            places: Arc::new(Semaphore::new(places.min(Semaphore::MAX_PERMITS))),
            wait,
        });
        self
    }

    /// Records the waits in [`WAITS`] and [`WAIT_DURATION`] and the
    /// refusals in [`REFUSALS`] of `meter`, such as `Telemetry::meter`'s.
    #[must_use]
    pub fn with_metrics(mut self, meter: &Meter) -> Self {
        self.metrics = Some(Metrics {
            waits: meter
                .u64_counter(WAITS)
                .with_description("Requests that waited for an admission slot.")
                .with_unit("{request}")
                .build(),
            waited: meter
                .f64_histogram(WAIT_DURATION)
                .with_description("Time a request waited for an admission slot.")
                .with_unit("s")
                .with_boundaries(WAIT_BUCKETS.to_vec())
                .build(),
            refusals: meter
                .u64_counter(REFUSALS)
                .with_description("Requests refused past the admission bound.")
                .with_unit("{request}")
                .build(),
        });
        self
    }

    /// Takes a slot, waiting in the queue when none is free.
    async fn slot(&self) -> Result<OwnedSemaphorePermit, Refusal> {
        // NOTE: tokio 1 `Semaphore` docs, "fair": a released permit goes to the first
        // waiter, so this try cannot take a slot ahead of a request already queued.
        if let Ok(slot) = Arc::clone(&self.slots).try_acquire_owned() {
            return Ok(slot);
        }
        let Some(queue) = &self.queue else {
            return Err(Refusal::QueueFull);
        };
        let Ok(_place) = Arc::clone(&queue.places).try_acquire_owned() else {
            return Err(Refusal::QueueFull);
        };
        if let Some(metrics) = &self.metrics {
            metrics.waits.add(1, &[]);
        }
        let started = tokio::time::Instant::now();
        let waited =
            tokio::time::timeout(queue.wait, Arc::clone(&self.slots).acquire_owned()).await;
        // NOTE: no specification governs this: our own design; the slots are never closed,
        // so an acquire error cannot occur and would be refused as an expired wait.
        let slot = match waited {
            Ok(Ok(slot)) => Ok(slot),
            Ok(Err(_)) | Err(_) => Err(Refusal::WaitExpired),
        };
        if let Some(metrics) = &self.metrics {
            let outcome = if slot.is_ok() { "admitted" } else { "expired" };
            metrics.waited.record(
                started.elapsed().as_secs_f64(),
                &[KeyValue::new(OUTCOME, outcome)],
            );
        }
        slot
    }

    /// The `503` that refuses a request for `refusal`.
    fn refuse(&self, refusal: Refusal) -> Response {
        if let Some(metrics) = &self.metrics {
            metrics
                .refusals
                .add(1, &[KeyValue::new(REASON, refusal.reason())]);
        }
        tracing::warn!(
            reason = refusal.reason(),
            "a request was refused: the server serves as many requests as it takes at once"
        );
        let mut response = crate::refusal::refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "the server is busy; retry later",
        );
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(retry_seconds(self.retry_after)),
        );
        response
    }
}

/// The middleware: serves the request in a slot, after a wait in the queue
/// when none is free, or refuses it.
pub async fn admit(State(admission): State<Admission>, request: Request, next: Next) -> Response {
    match admission.slot().await {
        Ok(_slot) => next.run(request).await,
        Err(refusal) => admission.refuse(refusal),
    }
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
