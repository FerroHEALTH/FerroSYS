// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The open-loop load: the request set sent at a fixed rate, whatever the
//! server answers.
//!
//! Arrival `i` is intended at `i / rate` seconds after the start, and takes
//! request `i` modulo the size of the set. The schedule runs on a thread of
//! its own that wakes at each intended instant to within tens of
//! microseconds (`pace`). Every latency runs from the intended instant, so a
//! late send and a server that stalls both raise the latency of every
//! request they held back (the coordinated-omission correction). The
//! generator's own lag, from the intended instant to the start of the send,
//! is recorded too, and a run whose generator could not keep its schedule is
//! refused.
//! An in-flight budget bounds the outstanding requests; an arrival that
//! waits longer than the saturation limit for a slot stops the run as
//! saturated. Only arrivals intended inside the measured window, after the
//! warm-up, are recorded, each in the half of the window that holds its
//! intended instant. The schedule and the coordinated-omission correction
//! follow Veredictum's `perf_gen` (Apache-2.0, credited in `NOTICE`).

mod pace;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;
use reqwest::Client;
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{timeout, timeout_at};
use url::Url;

use crate::credentials::Credentials;
use crate::histogram;
use crate::mllp::{Message, MllpError, Sender};
use crate::plan::{LoadSettings, Plan, Request};
use crate::quantile::exact_f64;
use crate::template::render::Renderer;
use crate::template::{TemplateError, Templates};

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
    /// The generator's lag over the measured window: how long after its
    /// intended instant each send started.
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
    /// A `DPoP` proof or a client assertion could not be signed.
    #[error(transparent)]
    Proof(crate::credentials::CredentialError),
    /// The plan sends HL7 v2 messages and the endpoints hold no MLLP
    /// listener for them.
    #[error("the plan sends HL7 v2 messages and no MLLP listener was set up for them")]
    NoListener,
    /// An arrival's variables could not be drawn.
    #[error("an arrival's variables cannot be drawn")]
    Template(#[source] TemplateError),
}

/// Endpoints that could not be set up.
#[derive(Debug, Error)]
pub enum EndpointsError {
    /// The MLLP listener of the plan's messages cannot be used.
    #[error(transparent)]
    Mllp(#[from] MllpError),
    /// A variable of the plan cannot be used.
    #[error("a variable of the plan cannot be used")]
    Template(#[from] TemplateError),
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
        .proof(position, &request.method, url, bearer(request))
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
    lag: Histogram<u64>,
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

/// Where a load phase sends, and what it sends with.
///
/// The server's base URL, its MLLP listener when the plan sends HL7 v2
/// messages, and the plan's variables with where each is in its sequence,
/// so the phases sent to one server continue them.
#[derive(Debug, Clone)]
pub struct Endpoints {
    url: Url,
    mllp: Option<Arc<Sender>>,
    templates: Option<Arc<Templates>>,
}

impl Endpoints {
    /// The endpoints of `plan` at `url`, with its messages sent to
    /// `mllp_address` or the plan's own `[mllp] address`.
    ///
    /// # Errors
    ///
    /// [`EndpointsError::Mllp`] for every error of [`Sender::new`], and
    /// [`EndpointsError::Template`] for a variable that cannot be used.
    pub fn new(plan: &Plan, url: Url, mllp_address: Option<&str>) -> Result<Self, EndpointsError> {
        Self::with(plan, url, Sender::new(plan, mllp_address)?)
    }

    /// The endpoints of [`Endpoints::new`], with every message file filled
    /// with `values` ([`crate::values`]).
    ///
    /// # Errors
    ///
    /// [`EndpointsError::Mllp`] for every error of [`Sender::filled`], and
    /// [`EndpointsError::Template`] for a variable that cannot be used.
    pub fn filled(
        plan: &Plan,
        url: Url,
        mllp_address: Option<&str>,
        values: &crate::values::Values,
    ) -> Result<Self, EndpointsError> {
        Self::with(plan, url, Sender::filled(plan, mllp_address, values)?)
    }

    fn with(plan: &Plan, url: Url, mllp: Option<Sender>) -> Result<Self, EndpointsError> {
        // NOTE: no specification governs this: our own design; a plan without variables is sent as written, never searched for placeholders.
        let templates = if plan.vars.is_empty() {
            None
        } else {
            let messages: Vec<&Message> = mllp
                .as_ref()
                .map(|sender| sender.messages().iter().map(Arc::as_ref).collect())
                .unwrap_or_default();
            Some(Arc::new(Templates::compile(plan, &messages)?))
        };
        Ok(Self {
            url,
            mllp: mllp.map(Arc::new),
            templates,
        })
    }

    /// The MLLP listener's address, when the plan sends messages.
    #[must_use]
    pub fn mllp_address(&self) -> Option<&str> {
        self.mllp.as_deref().map(Sender::address)
    }
}

/// One entry of the set an arrival takes.
#[derive(Debug)]
enum Item {
    Http {
        position: usize,
        request: Box<Request>,
        url: Url,
    },
    Mllp {
        index: usize,
        group: String,
        sender: Arc<Sender>,
        message: Arc<Message>,
    },
}

impl Item {
    /// The group and the exchange of one send of this item, its variables
    /// drawn by `renderer` and its URL below `base`.
    fn exchange(
        &self,
        client: &Client,
        credentials: &Credentials,
        base: &Url,
        renderer: Option<&mut Renderer<'_>>,
    ) -> Result<(&str, Exchange), LoadError> {
        Ok(match self {
            Self::Http {
                position,
                request,
                url,
            } => {
                let filled = match renderer {
                    Some(renderer) => renderer
                        .request(*position, request)
                        .map_err(LoadError::Template)?,
                    None => None,
                };
                let (mut request, url) = match filled {
                    Some(filled) => {
                        let url = request_url(base, &filled)?;
                        (filled, url)
                    }
                    None => ((**request).clone(), url.clone()),
                };
                // NOTE: RFC 7523 §3: an assertion is used once; it is signed before the intended instant, outside the timed window.
                if let Some(fields) = credentials
                    .assertion(*position, &url)
                    .map_err(LoadError::Proof)?
                {
                    request.body = Some(match request.body.take() {
                        Some(body) if !body.is_empty() => format!("{body}&{fields}"),
                        _ => fields,
                    });
                }
                // NOTE: RFC 9449 §4.2: a proof is fresh per request; it is signed before the intended instant, outside the timed window.
                let proof = sign(credentials, *position, &request, &url)?;
                let call = HttpCall {
                    client: client.clone(),
                    request,
                    url,
                    proof,
                };
                (self.group(), Exchange::Http(Box::new(call)))
            }
            Self::Mllp {
                index,
                group,
                sender,
                message,
            } => {
                let filled = match renderer {
                    Some(renderer) => renderer.message(*index).map_err(LoadError::Template)?,
                    None => None,
                };
                let exchange = Exchange::Mllp {
                    sender: Arc::clone(sender),
                    message: filled.map_or_else(|| Arc::clone(message), Arc::new),
                };
                (group.as_str(), exchange)
            }
        })
    }

    /// The group its latency is recorded under.
    fn group(&self) -> &str {
        match self {
            Self::Http { request, .. } => &request.group,
            Self::Mllp { group, .. } => group,
        }
    }
}

/// The set of `plan` at `endpoints`: the requests, then the messages.
fn items(plan: &Plan, endpoints: &Endpoints) -> Result<Vec<Item>, LoadError> {
    let mut items = Vec::with_capacity(plan.requests.len().saturating_add(plan.messages.len()));
    for (position, request) in plan.requests.iter().enumerate() {
        let url = request_url(&endpoints.url, request)?;
        items.push(Item::Http {
            position,
            request: Box::new(request.clone()),
            url,
        });
    }
    for (index, entry) in plan.messages.iter().enumerate() {
        let sender = endpoints.mllp.as_ref().ok_or(LoadError::NoListener)?;
        let message = sender.message(index).ok_or(LoadError::NoListener)?;
        items.push(Item::Mllp {
            index,
            group: entry.group.clone(),
            sender: Arc::clone(sender),
            message: Arc::clone(message),
        });
    }
    Ok(items)
}

/// Drives the load phase of `plan` against `endpoints`, starting at
/// `start`.
///
/// The schedule runs on a blocking thread of the current runtime and sends
/// each arrival as a task on that runtime; dropping the returned future
/// stops the schedule at its next arrival.
///
/// # Errors
///
/// [`LoadError::Url`] for a request that forms no URL,
/// [`LoadError::NoListener`] for messages with no MLLP listener,
/// [`LoadError::Schedule`] for a schedule too long to place,
/// [`LoadError::Proof`] for a `DPoP` proof that cannot be signed, and
/// [`LoadError::Task`] when a request task or the schedule's thread panics.
pub async fn drive(
    plan: &Plan,
    endpoints: &Endpoints,
    client: &Client,
    credentials: &Credentials,
    start: tokio::time::Instant,
) -> Result<LoadOutcome, LoadError> {
    let load = plan.load.clone();
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
        lag: histogram::empty(),
    }));
    let stop = Stop::default();
    let schedule = Schedule {
        items: items(plan, endpoints)?,
        base: endpoints.url.clone(),
        templates: endpoints.templates.clone(),
        client: client.clone(),
        credentials: credentials.clone(),
        start: start.into_std(),
        shared: Arc::clone(&shared),
        stop: Arc::clone(&stop.0),
        runtime: Handle::current(),
        load: load.clone(),
    };
    let Paced {
        mut tasks,
        planned,
        sent,
        saturated,
    } = tokio::task::spawn_blocking(move || schedule.run())
        .await
        .map_err(LoadError::Task)??;
    // Every request ends by its own timeout; the drain waits one second past
    // it, so only a request its timeout could not stop counts as unfinished.
    let drain = Duration::from_millis(load.timeout_ms).saturating_add(Duration::from_secs(1));
    let deadline = tokio::time::Instant::now()
        .checked_add(drain)
        .unwrap_or_else(tokio::time::Instant::now);
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
        lag: std::mem::replace(&mut shared.lag, histogram::empty()),
        planned,
        sent,
        saturated,
    })
}

/// The flag that stops the schedule, raised when the phase's future is
/// dropped.
#[derive(Debug, Default)]
struct Stop(Arc<AtomicBool>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// What the schedule's thread owns.
#[derive(Debug)]
struct Schedule {
    load: LoadSettings,
    items: Vec<Item>,
    base: Url,
    templates: Option<Arc<Templates>>,
    client: Client,
    credentials: Credentials,
    start: Instant,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    runtime: Handle,
}

/// What the schedule's thread hands back: the request tasks still running
/// and the counts.
#[derive(Debug)]
struct Paced {
    tasks: JoinSet<()>,
    planned: u64,
    sent: u64,
    saturated: bool,
}

impl Schedule {
    /// Sends every arrival at its intended instant, on the calling thread,
    /// which it blocks.
    fn run(self) -> Result<Paced, LoadError> {
        let load = &self.load;
        let span = load.warmup().saturating_add(load.duration());
        let planned = arrivals(span, load.rate);
        let budget = usize::try_from(load.max_in_flight).unwrap_or(usize::MAX);
        let semaphore = Arc::new(Semaphore::new(budget.min(Semaphore::MAX_PERMITS)));
        let request_timeout = Duration::from_millis(load.timeout_ms);
        let saturation = Duration::from_millis(load.saturation_ms);
        let mut pacer = pace::Pacer::default();
        let mut tasks = JoinSet::new();
        let mut sent: u64 = 0;
        let mut saturated = false;
        let mut order = self.items.iter().cycle();
        let mut renderer = self.templates.as_deref().map(Templates::renderer);
        for index in 0..planned {
            if self.stop.load(Ordering::Acquire) {
                break;
            }
            let offset = Duration::try_from_secs_f64(exact_f64(index) / load.rate)
                .map_err(|_too_long| LoadError::Schedule(planned))?;
            let intended = self
                .start
                .checked_add(offset)
                .ok_or(LoadError::Schedule(planned))?;
            let Some(item) = order.next() else {
                break;
            };
            // NOTE: no specification governs this: our own design; the variables are drawn before the wait, so drawing them moves no send.
            let (group, exchange) = item.exchange(
                &self.client,
                &self.credentials,
                &self.base,
                renderer.as_mut(),
            )?;
            pacer.wait_until(intended);
            let permit = if let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() {
                permit
            } else if let Ok(Ok(permit)) = self
                .runtime
                .block_on(timeout(saturation, Arc::clone(&semaphore).acquire_owned()))
            {
                permit
            } else {
                saturated = true;
                break;
            };
            let call = Call {
                group: group.to_owned(),
                exchange,
                intended,
                window: window_of(offset, load),
                request_timeout,
            };
            self.shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pending(group, true);
            let shared = Arc::clone(&self.shared);
            tasks.spawn_on(
                async move {
                    call.send(&shared).await;
                    drop(permit);
                },
                &self.runtime,
            );
            sent = sent.saturating_add(1);
            while let Some(result) = tasks.try_join_next() {
                result.map_err(LoadError::Task)?;
            }
        }
        Ok(Paced {
            tasks,
            planned,
            sent,
            saturated,
        })
    }
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
        let began = Instant::now();
        let outcome = match &self.exchange {
            Exchange::Http(call) => call.send(self.request_timeout).await,
            Exchange::Mllp { sender, message } => sender.send(message, self.request_timeout).await,
        };
        let done = Instant::now();
        let mut shared = shared.lock().unwrap_or_else(PoisonError::into_inner);
        shared.pending(&self.group, false);
        if self.window != Window::Warmup {
            let lag = began.saturating_duration_since(self.intended);
            histogram::record(&mut shared.lag, lag);
        }
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
            client_assertion: None,
        };
        let base = Url::parse("http://127.0.0.1:8080/api/").unwrap();
        assert_eq!(
            "http://127.0.0.1:8080/api/fhir/metadata?_format=json",
            request_url(&base, &request).unwrap().as_str()
        );
    }
}
