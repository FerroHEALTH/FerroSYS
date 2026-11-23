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
//! server is not restarted by its own probe.
//!
//! The stack tries the slot before the request id, the request log and the
//! body limit, so a refusal costs less than serving: it leaves no request
//! line and reads no body. Every refusal is counted in [`REFUSALS`] by its
//! reason, and the console has at most one line a second with the count of
//! the refusals since the line before. No specification governs the queue,
//! its sizes or the refusal line: our own design.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::{HeaderValue, StatusCode, header};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::refusal::RefusalBody;
use crate::request_id::{self, RequestIds};

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

/// The reason in plain words a refused request is told.
const BUSY: &str = "the server is busy; retry later";

/// The shortest time between two refusal lines.
const LINE_INTERVAL: Duration = Duration::from_secs(1);

/// The bucket boundaries of [`WAIT_DURATION`], in seconds, around the
/// default wait of 100 ms.
const WAIT_BUCKETS: [f64; 12] = [
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 1.0, 2.5,
];

/// The admission: the slots, the queue in front of them, and the wait a
/// refused request is told.
#[derive(Clone)]
pub struct Admission {
    slots: Arc<Semaphore>,
    queue: Option<Queue>,
    retry_after: Duration,
    metrics: Option<Metrics>,
    exempt: Arc<[Box<str>]>,
    body: Option<RefusalBody>,
    lines: Arc<RefusalLines>,
}

impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admission")
            .field("slots", &self.slots)
            .field("queue", &self.queue)
            .field("retry_after", &self.retry_after)
            .field("metrics", &self.metrics)
            .field("exempt", &self.exempt)
            .field("lines", &self.lines)
            .finish_non_exhaustive()
    }
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

/// The refusals since the last refusal line, and when that line was
/// written.
#[derive(Debug)]
struct RefusalLines {
    /// The instant the times below count from.
    origin: Instant,
    /// One more than the nanoseconds from `origin` to the last line, or zero
    /// before the first.
    last: AtomicU64,
    /// The refusals since the last line, for a full queue and for an expired
    /// wait.
    queue_full: AtomicU64,
    wait_expired: AtomicU64,
}

impl RefusalLines {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            last: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            wait_expired: AtomicU64::new(0),
        }
    }

    /// Counts `refusal`, and writes the line with the counts when the last
    /// line is at least [`LINE_INTERVAL`] old.
    fn note(&self, refusal: Refusal) {
        let pending = match refusal {
            Refusal::QueueFull => &self.queue_full,
            Refusal::WaitExpired => &self.wait_expired,
        };
        pending.fetch_add(1, Ordering::Relaxed);
        let now = u64::try_from(self.origin.elapsed().as_nanos())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let last = self.last.load(Ordering::Relaxed);
        let interval = u64::try_from(LINE_INTERVAL.as_nanos()).unwrap_or(u64::MAX);
        if last != 0 && now.saturating_sub(last) < interval {
            return;
        }
        // NOTE: no specification governs this: our own design; of the refusals that
        // find the line due at once, the one that moves `last` writes it.
        if self
            .last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let queue_full = self.queue_full.swap(0, Ordering::Relaxed);
        let wait_expired = self.wait_expired.swap(0, Ordering::Relaxed);
        tracing::warn!(
            queue_full,
            wait_expired,
            "requests were refused since the last such line: the server serves as many \
             requests as it takes at once"
        );
    }
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
            exempt: Arc::new([]),
            body: None,
            lines: Arc::new(RefusalLines::new()),
        }
    }

    /// Passes every request whose matched route is one of `routes` without
    /// a slot, such as the health family's.
    pub(crate) fn exempting(mut self, routes: impl IntoIterator<Item = String>) -> Self {
        self.exempt = routes.into_iter().map(String::into_boxed_str).collect();
        self
    }

    /// Writes each refusal with `body` and the request id header, for a
    /// bound that runs before the stack's request id and refusal rendering.
    pub(crate) fn writing(mut self, body: RefusalBody) -> Self {
        self.body = Some(body);
        self
    }

    /// Whether `request` passes without a slot.
    fn exempts(&self, request: &Request) -> bool {
        !self.exempt.is_empty()
            && request
                .extensions()
                .get::<MatchedPath>()
                .is_some_and(|matched| self.exempt.iter().any(|route| **route == *matched.as_str()))
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
        let started = Instant::now();
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

    /// The `503` that refuses `request` for `refusal`.
    fn refuse(&self, refusal: Refusal, request: &Request) -> Response {
        if let Some(metrics) = &self.metrics {
            metrics
                .refusals
                .add(1, &[KeyValue::new(REASON, refusal.reason())]);
        }
        self.lines.note(refusal);
        let mut response = match &self.body {
            Some(body) => {
                let client = request_id::client_value(request.headers());
                let server = request_id::mint_id(client.is_some());
                let echoed = client.as_ref().and_then(|client| client.to_str().ok());
                let mut response = crate::refusal::write(
                    body,
                    request.uri(),
                    RequestIds::new(server.id(), echoed),
                    StatusCode::SERVICE_UNAVAILABLE,
                    BUSY,
                );
                let echoed = client.or_else(|| HeaderValue::from_str(server.id()).ok());
                if let Some(echoed) = echoed {
                    response.headers_mut().insert(request_id::HEADER, echoed);
                }
                response
            }
            None => crate::refusal::refused(StatusCode::SERVICE_UNAVAILABLE, BUSY),
        };
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(retry_seconds(self.retry_after)),
        );
        response
    }
}

/// The middleware: serves the request in a slot, after a wait in the queue
/// when none is free, or refuses it. A request to an exempt route, the
/// stack's health family, is served without a slot.
pub async fn admit(State(admission): State<Admission>, request: Request, next: Next) -> Response {
    if admission.exempts(&request) {
        return next.run(request).await;
    }
    match admission.slot().await {
        Ok(_slot) => next.run(request).await,
        Err(refusal) => admission.refuse(refusal, &request),
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
