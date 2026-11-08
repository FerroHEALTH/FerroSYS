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
use crate::mllp::{Message, MllpError, Sender};
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
    /// The plan sends HL7 v2 messages and the endpoints hold no MLLP
    /// listener for them.
    #[error("the plan sends HL7 v2 messages and no MLLP listener was set up for them")]
    NoListener,
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

/// Where a load phase sends: the server's base URL, and its MLLP listener
/// when the plan sends HL7 v2 messages.
#[derive(Debug, Clone)]
pub struct Endpoints {
    url: Url,
    mllp: Option<Arc<Sender>>,
}

impl Endpoints {
    /// The endpoints of `plan` at `url`, with its messages sent to
    /// `mllp_address` or the plan's own `[mllp] address`.
    ///
    /// # Errors
    ///
    /// Every [`MllpError`] of [`Sender::new`].
    pub fn new(plan: &Plan, url: Url, mllp_address: Option<&str>) -> Result<Self, MllpError> {
        let mllp = Sender::new(plan, mllp_address)?.map(Arc::new);
        Ok(Self { url, mllp })
    }

    /// The endpoints of [`Endpoints::new`], with every message file filled
    /// with `values` ([`crate::values`]).
    ///
    /// # Errors
    ///
    /// Every [`MllpError`] of [`Sender::filled`].
    pub fn filled(
        plan: &Plan,
        url: Url,
        mllp_address: Option<&str>,
        values: &crate::values::Values,
    ) -> Result<Self, MllpError> {
        let mllp = Sender::filled(plan, mllp_address, values)?.map(Arc::new);
        Ok(Self { url, mllp })
    }

    /// The MLLP listener's address, when the plan sends messages.
    #[must_use]
    pub fn mllp_address(&self) -> Option<&str> {
        self.mllp.as_deref().map(Sender::address)
    }
}

/// One entry of the set an arrival takes.
#[derive(Debug)]
enum Item<'a> {
    Http {
        position: usize,
        request: &'a Request,
        url: Url,
    },
    Mllp {
        group: &'a str,
        sender: Arc<Sender>,
        message: Arc<Message>,
    },
}

impl<'a> Item<'a> {
    /// The group and the exchange of one send of this item.
    fn exchange(
        &self,
        client: &Client,
        credentials: &Credentials,
    ) -> Result<(&'a str, Exchange), LoadError> {
        Ok(match self {
            Self::Http {
                position,
                request,
                url,
            } => {
                // NOTE: RFC 9449 §4.2: a proof is fresh per request; it is signed before the intended instant, outside the timed window.
                let proof = sign(credentials, *position, request, url)?;
                let call = HttpCall {
                    client: client.clone(),
                    request: (*request).clone(),
                    url: url.clone(),
                    proof,
                };
                (request.group.as_str(), Exchange::Http(Box::new(call)))
            }
            Self::Mllp {
                group,
                sender,
                message,
            } => {
                let exchange = Exchange::Mllp {
                    sender: Arc::clone(sender),
                    message: Arc::clone(message),
                };
                (*group, exchange)
            }
        })
    }
}

/// The set of `plan` at `endpoints`: the requests, then the messages.
fn items<'a>(plan: &'a Plan, endpoints: &Endpoints) -> Result<Vec<Item<'a>>, LoadError> {
    let mut items = Vec::with_capacity(plan.requests.len().saturating_add(plan.messages.len()));
    for (position, request) in plan.requests.iter().enumerate() {
        let url = request_url(&endpoints.url, request)?;
        items.push(Item::Http {
            position,
            request,
            url,
        });
    }
    for (index, entry) in plan.messages.iter().enumerate() {
        let sender = endpoints.mllp.as_ref().ok_or(LoadError::NoListener)?;
        let message = sender.message(index).ok_or(LoadError::NoListener)?;
        items.push(Item::Mllp {
            group: &entry.group,
            sender: Arc::clone(sender),
            message: Arc::clone(message),
        });
    }
    Ok(items)
}

/// Drives the load phase of `plan` against `endpoints`, starting at
/// `start`.
///
/// # Errors
///
/// [`LoadError::Url`] for a request that forms no URL,
/// [`LoadError::NoListener`] for messages with no MLLP listener,
/// [`LoadError::Schedule`] for a schedule too long to place, and
/// [`LoadError::Task`] when a request task panics.
pub async fn drive(
    plan: &Plan,
    endpoints: &Endpoints,
    client: &Client,
    credentials: &Credentials,
    start: Instant,
) -> Result<LoadOutcome, LoadError> {
    let load = &plan.load;
    let items = items(plan, endpoints)?;
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
    let mut order = items.iter().cycle();
    for index in 0..planned {
        let offset = Duration::try_from_secs_f64(exact_f64(index) / load.rate)
            .map_err(|_too_long| LoadError::Schedule(planned))?;
        let intended = start
            .checked_add(offset)
            .ok_or(LoadError::Schedule(planned))?;
        let Some(item) = order.next() else {
            break;
        };
        let (group, exchange) = item.exchange(client, credentials)?;
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
            group: group.to_owned(),
            exchange,
            intended,
            window,
            request_timeout,
        };
        shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending(group, true);
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

/// One HTTP request of an arrival, with its `DPoP` proof.
#[derive(Debug)]
struct HttpCall {
    client: Client,
    request: Request,
    url: Url,
    proof: Option<String>,
}

/// What one arrival sends.
#[derive(Debug)]
enum Exchange {
    Http(Box<HttpCall>),
    Mllp {
        sender: Arc<Sender>,
        message: Arc<Message>,
    },
}

#[derive(Debug)]
struct Call {
    group: String,
    exchange: Exchange,
    intended: Instant,
    window: Window,
    request_timeout: Duration,
}

impl Call {
    async fn send(self, shared: &Mutex<Shared>) {
        let outcome = match &self.exchange {
            Exchange::Http(call) => call.send(self.request_timeout).await,
            Exchange::Mllp { sender, message } => sender.send(message, self.request_timeout).await,
        };
        let done = Instant::now();
        let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.pending(&self.group, false);
        match outcome {
            Ok(()) => {
                let latency = done.saturating_duration_since(self.intended);
                let Some(record) = shared.groups.get_mut(&self.group) else {
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
            Err(kind) => shared.fail(&self.group, kind),
        }
    }
}

impl HttpCall {
    /// Sends the request and reads the whole answer; the failure kind when the
    /// answer is not the expected one.
    async fn send(&self, request_timeout: Duration) -> Result<(), String> {
        let HttpCall {
            client,
            request,
            url,
            proof,
        } = self;
        let method = request.method()?;
        let mut builder = client.request(method, url.clone()).timeout(request_timeout);
        for (name, value) in &request.headers {
            match (proof, bearer(request)) {
                // NOTE: RFC 9449 §7.1: a DPoP-bound token goes out under the DPoP scheme.
                (Some(_), Some(token)) if name.eq_ignore_ascii_case("authorization") => {
                    builder = builder.header(name, format!("DPoP {token}"));
                }
                _ => builder = builder.header(name, value),
            }
        }
        if let Some(proof) = proof {
            builder = builder.header("dpop", proof.as_str());
        }
        if let Some(body) = &request.body {
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
        let expected = request.expected()?;
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
