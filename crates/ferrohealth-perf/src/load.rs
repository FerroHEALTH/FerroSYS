// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The open-loop load: the request set sent at a fixed rate, whatever the
//! server answers.
//!
//! Arrival `i` is intended at `i / rate` seconds after the start, and takes
//! request `i` modulo the size of the set. Every latency runs from the
//! intended instant, so a server that stalls raises the latency of every
//! request it held back (the coordinated-omission correction). The
//! generator's own lag, from the intended instant to the send, is recorded
//! too, and a run whose generator could not keep its schedule is refused.
//! An in-flight budget bounds the outstanding requests; an arrival that
//! waits longer than the saturation limit for a slot stops the run as
//! saturated. Only arrivals intended inside the measured window, after the
//! warm-up, are recorded, each in the half of the window that holds its
//! intended instant. The schedule and the coordinated-omission correction
//! follow Veredictum's `perf_gen` (Apache-2.0, credited in `NOTICE`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hdrhistogram::Histogram;
use reqwest::Client;
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use url::Url;

use crate::credentials::Credentials;
use crate::histogram;
use crate::plan::{LoadSettings, Plan, Request};
use crate::quantile::exact_f64;

/// What one group recorded over the measured window.
#[derive(Debug, Clone)]
pub struct GroupRecord {
    /// The latencies of the arrivals intended in the first half.
    pub first: Histogram<u64>,
    /// The latencies of the arrivals intended in the second half.
    pub second: Histogram<u64>,
    /// The requests answered with the expected status.
    pub answered: u64,
}

/// The outcome of one load phase.
#[derive(Debug, Clone)]
pub struct LoadOutcome {
    /// The record of each group, by name.
    pub groups: BTreeMap<String, GroupRecord>,
    /// The failures over the whole phase, warm-up included, by group and
    /// kind (`status 500`, `timeout`, `transport`, `unfinished`).
    pub failures: BTreeMap<String, BTreeMap<String, u64>>,
    /// The generator's lag over the measured window.
    pub lag: Histogram<u64>,
    /// The arrivals the schedule held.
    pub planned: u64,
    /// The arrivals sent.
    pub sent: u64,
    /// Whether an arrival waited past the saturation limit for a slot.
    pub saturated: bool,
}

/// A load phase that could not run.
#[derive(Debug, Error)]
pub enum LoadError {
    /// A request of the set does not form a URL below the base.
    #[error("the request path {path} does not form a URL below the base")]
    Url {
        /// The path.
        path: String,
        /// The cause.
        #[source]
        source: url::ParseError,
    },
    /// A request task panicked.
    #[error("a request task failed")]
    Task(#[source] tokio::task::JoinError),
    /// The schedule does not fit in a run.
    #[error("the schedule of {0} arrivals is too long")]
    Schedule(u64),
    /// A `DPoP` proof could not be signed.
    #[error(transparent)]
    Proof(crate::credentials::CredentialError),
}

/// A fresh `DPoP` proof for request `position` of the plan sent to `url`,
/// when the request names a key.
fn sign(
    credentials: &Credentials,
    position: usize,
    request: &Request,
    url: &Url,
) -> Result<Option<String>, LoadError> {
    credentials
        .dpop(position)
        .map(|key| key.proof(request.method.as_str(), url, bearer(request)))
        .transpose()
        .map_err(LoadError::Proof)
}

/// The bearer token of `request`'s `Authorization` header, if it has one.
fn bearer(request: &Request) -> Option<&str> {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| {
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then_some(token.trim())
        })
}

#[derive(Debug)]
struct Shared {
    groups: BTreeMap<String, GroupRecord>,
    failures: BTreeMap<String, BTreeMap<String, u64>>,
    pending: BTreeMap<String, u64>,
}

impl Shared {
    fn pending(&mut self, group: &str, sent: bool) {
        let count = self.pending.entry(group.to_owned()).or_insert(0);
        *count = if sent {
            count.saturating_add(1)
        } else {
            count.saturating_sub(1)
        };
    }

    fn fail(&mut self, group: &str, kind: String) {
        let count = self
            .failures
            .entry(group.to_owned())
            .or_default()
            .entry(kind)
            .or_insert(0);
        *count = count.saturating_add(1);
    }
}

/// Where an arrival's intended instant falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Window {
    Warmup,
    First,
    Second,
}

fn window_of(offset: Duration, load: &LoadSettings) -> Window {
    let warmup = load.warmup();
    let half = warmup.saturating_add(load.duration() / 2);
    if offset < warmup {
        Window::Warmup
    } else if offset < half {
        Window::First
    } else {
        Window::Second
    }
}

/// The URL of `request` below `base`: the base with its trailing `/`
/// removed, then the request's path, so a base with a path prefix keeps it.
///
/// # Errors
///
/// [`LoadError::Url`] when the joined text is not a URL.
pub fn request_url(base: &Url, request: &Request) -> Result<Url, LoadError> {
    let joined = format!("{}{}", base.as_str().trim_end_matches('/'), request.path);
    Url::parse(&joined).map_err(|source| LoadError::Url {
        path: request.path.clone(),
        source,
    })
}

/// Drives the load phase of `plan` against `base`, starting at `start`.
///
/// # Errors
///
/// [`LoadError::Url`] for a request that forms no URL,
/// [`LoadError::Schedule`] for a schedule too long to place, and
/// [`LoadError::Task`] when a request task panics.
pub async fn drive(
    plan: &Plan,
    base: &Url,
    client: &Client,
    credentials: &Credentials,
    start: Instant,
) -> Result<LoadOutcome, LoadError> {
    let load = &plan.load;
    let urls = plan
        .requests
        .iter()
        .map(|request| request_url(base, request))
        .collect::<Result<Vec<_>, _>>()?;
    let record = || GroupRecord {
        first: histogram::empty(),
        second: histogram::empty(),
        answered: 0,
    };
    let groups = plan.groups().into_iter().map(|g| (g, record())).collect();
    let shared = Arc::new(Mutex::new(Shared {
        groups,
        failures: BTreeMap::new(),
        pending: BTreeMap::new(),
    }));
    let mut lag = histogram::empty();
    let span = load.warmup().saturating_add(load.duration());
    let planned = arrivals(span, load.rate);
    let budget = usize::try_from(load.max_in_flight).unwrap_or(usize::MAX);
    let semaphore = Arc::new(Semaphore::new(budget.min(Semaphore::MAX_PERMITS)));
    let request_timeout = Duration::from_millis(load.timeout_ms);
    let saturation = Duration::from_millis(load.saturation_ms);
    let mut tasks = JoinSet::new();
    let mut sent: u64 = 0;
    let mut saturated = false;
    let mut order = plan.requests.iter().zip(urls.iter()).enumerate().cycle();
    for index in 0..planned {
        let offset = Duration::try_from_secs_f64(exact_f64(index) / load.rate)
            .map_err(|_too_long| LoadError::Schedule(planned))?;
        let intended = start
            .checked_add(offset)
            .ok_or(LoadError::Schedule(planned))?;
        let Some((position, (request, url))) = order.next() else {
            break;
        };
        // NOTE: RFC 9449 §4.2: a proof is fresh per request; it is signed before the intended instant, outside the timed window.
        let proof = sign(credentials, position, request, url)?;
        sleep_until(intended).await;
        let permit = if let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() {
            permit
        } else if let Ok(Ok(permit)) =
            timeout(saturation, Arc::clone(&semaphore).acquire_owned()).await
        {
            permit
        } else {
            saturated = true;
            break;
        };
        let window = window_of(offset, load);
        if window != Window::Warmup {
            histogram::record(&mut lag, Instant::now().saturating_duration_since(intended));
        }
        let call = Call {
            client: client.clone(),
            request: request.clone(),
            url: url.clone(),
            intended,
            window,
            request_timeout,
            proof,
        };
        shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending(&request.group, true);
        let shared = Arc::clone(&shared);
        tasks.spawn(async move {
            call.send(&shared).await;
            drop(permit);
        });
        sent = sent.saturating_add(1);
        while let Some(result) = tasks.try_join_next() {
            result.map_err(LoadError::Task)?;
        }
    }
    // Every request ends by its own timeout; the drain waits one second past
    // it, so only a request its timeout could not stop counts as unfinished.
    let drain = request_timeout.saturating_add(Duration::from_secs(1));
    let deadline = Instant::now()
        .checked_add(drain)
        .unwrap_or_else(Instant::now);
    while let Ok(Some(result)) = timeout_at(deadline, tasks.join_next()).await {
        result.map_err(LoadError::Task)?;
    }
    tasks.abort_all();
    let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let pending = std::mem::take(&mut shared.pending);
    for (group, count) in pending.into_iter().filter(|(_, count)| *count > 0) {
        let entry = shared.failures.entry(group).or_default();
        entry.insert("unfinished".to_owned(), count);
    }
    Ok(LoadOutcome {
        groups: std::mem::take(&mut shared.groups),
        failures: std::mem::take(&mut shared.failures),
        lag,
        planned,
        sent,
        saturated,
    })
}

/// The arrivals of a schedule of `span` at `rate` per second.
fn arrivals(span: Duration, rate: f64) -> u64 {
    let count = (span.as_secs_f64() * rate).floor();
    let mut planned: u64 = 0;
    // The count is a whole number at most span·rate; it is found by
    // doubling and halving so no float is cast to an integer.
    let mut step: u64 = 1 << 62;
    while step > 0 {
        let next = planned.saturating_add(step);
        if exact_f64(next) <= count {
            planned = next;
        }
        step >>= 1;
    }
    planned
}

#[derive(Debug)]
struct Call {
    client: Client,
    request: Request,
    url: Url,
    intended: Instant,
    window: Window,
    request_timeout: Duration,
    proof: Option<String>,
}

impl Call {
    async fn send(self, shared: &Mutex<Shared>) {
        let outcome = self.exchange().await;
        let done = Instant::now();
        let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.pending(&self.request.group, false);
        match outcome {
            Ok(()) => {
                let latency = done.saturating_duration_since(self.intended);
                let Some(record) = shared.groups.get_mut(&self.request.group) else {
                    return;
                };
                match self.window {
                    Window::Warmup => {}
                    Window::First => {
                        histogram::record(&mut record.first, latency);
                        record.answered = record.answered.saturating_add(1);
                    }
                    Window::Second => {
                        histogram::record(&mut record.second, latency);
                        record.answered = record.answered.saturating_add(1);
                    }
                }
            }
            Err(kind) => shared.fail(&self.request.group, kind),
        }
    }

    /// Sends the request and reads the whole answer; the failure kind when
    /// the answer is not the expected one.
    async fn exchange(&self) -> Result<(), String> {
        let method = self.request.method()?;
        let mut builder = self
            .client
            .request(method, self.url.clone())
            .timeout(self.request_timeout);
        for (name, value) in &self.request.headers {
            match (&self.proof, bearer(&self.request)) {
                // NOTE: RFC 9449 §7.1: a DPoP-bound token goes out under the DPoP scheme.
                (Some(_), Some(token)) if name.eq_ignore_ascii_case("authorization") => {
                    builder = builder.header(name, format!("DPoP {token}"));
                }
                _ => builder = builder.header(name, value),
            }
        }
        if let Some(proof) = &self.proof {
            builder = builder.header("dpop", proof);
        }
        if let Some(body) = &self.request.body {
            builder = builder.body(body.clone());
        }
        let kind = |error: &reqwest::Error| {
            if error.is_timeout() {
                "timeout".to_owned()
            } else {
                "transport".to_owned()
            }
        };
        let response = builder.send().await.map_err(|error| kind(&error))?;
        let status = response.status();
        response.bytes().await.map_err(|error| kind(&error))?;
        let expected = self.request.expected()?;
        if status == expected {
            Ok(())
        } else {
            Err(format!("status {}", status.as_u16()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schedule_holds_span_times_rate_arrivals() {
        assert_eq!(150, arrivals(Duration::from_secs(3), 50.0));
        assert_eq!(1, arrivals(Duration::from_millis(1500), 1.0));
        assert_eq!(0, arrivals(Duration::from_millis(500), 1.0));
    }

    #[test]
    fn an_offset_falls_in_the_warm_up_or_a_half() {
        let load = LoadSettings {
            warmup_secs: 2,
            duration_secs: 4,
            ..LoadSettings::default()
        };
        assert_eq!(
            Window::Warmup,
            window_of(Duration::from_millis(1_999), &load)
        );
        assert_eq!(Window::First, window_of(Duration::from_secs(2), &load));
        assert_eq!(
            Window::First,
            window_of(Duration::from_millis(3_999), &load)
        );
        assert_eq!(Window::Second, window_of(Duration::from_secs(4), &load));
    }

    #[test]
    fn a_base_with_a_path_keeps_it() {
        let request = Request {
            group: "g".to_owned(),
            method: "GET".to_owned(),
            path: "/fhir/metadata?_format=json".to_owned(),
            status: 200,
            body: None,
            headers: BTreeMap::new(),
            dpop_key_file: None,
        };
        let base = Url::parse("http://127.0.0.1:8080/api/").unwrap();
        assert_eq!(
            "http://127.0.0.1:8080/api/fhir/metadata?_format=json",
            request_url(&base, &request).unwrap().as_str()
        );
    }
}
