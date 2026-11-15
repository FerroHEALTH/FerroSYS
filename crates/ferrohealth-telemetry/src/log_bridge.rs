// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The process subscriber, installed with the bridge that carries the `log`
//! crate's records into it.
//!
//! A `log` record becomes the event of a callsite registered for its target
//! and level, so each subscriber layer decides on it once per callsite, the
//! way it decides on a `tracing` macro, and the console filter reaches a
//! record through the dispatch every event takes. The bridge answers
//! `log::Log::enabled` from the global `tracing` maximum level alone and asks
//! no subscriber: `tracing-subscriber` 0.3.23 leaves a per-layer filter's
//! "disabled" decision on the thread when a bare `Dispatch::enabled` question
//! is answered false, and the next event of a callsite whose interest it
//! cached is then dropped (#388, #389). No specification governs this: our own
//! design.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

use tracing::field::{FieldSet, Value};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber, dispatcher};
use tracing_core::callsite::{self, Callsite};
use tracing_core::metadata::{Kind, LevelFilter};

use crate::telemetry::InitError;

/// The one field of a bridged record: its formatted message.
const FIELDS: &[&str] = &["message"];

/// A callsite's interest before the registry set one, and each it can set.
const UNREGISTERED: u8 = 0;
const NEVER: u8 = 1;
const SOMETIMES: u8 = 2;
const ALWAYS: u8 = 3;

/// Installs `subscriber` as the process subscriber and the bridge as the
/// `log` crate's logger.
///
/// Every product installs its subscriber through this function or through
/// [`crate::telemetry::Telemetry::install`], so no other `log` bridge (such
/// as `tracing-log`'s `LogTracer`) sits in front of it.
///
/// # Errors
///
/// Returns [`InitError::Subscriber`] when the process has a subscriber and
/// [`InitError::Logger`] when it has a `log` logger.
pub fn install<S>(subscriber: S) -> Result<(), InitError>
where
    S: Subscriber + Send + Sync + 'static,
{
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|source| InitError::Subscriber { source })?;
    log::set_boxed_logger(Box::new(LogBridge::default()))
        .map_err(|source| InitError::Logger { source })?;
    // NOTE: no specification governs this: our own design; the bridge itself
    // compares a record with the current maximum, which a filter reload moves.
    log::set_max_level(log::LevelFilter::Trace);
    Ok(())
}

/// The `log` logger that dispatches each record as a `tracing` event.
#[derive(Debug, Default)]
struct LogBridge {
    error: RwLock<BTreeMap<Box<str>, &'static LogCallsite>>,
    warn: RwLock<BTreeMap<Box<str>, &'static LogCallsite>>,
    info: RwLock<BTreeMap<Box<str>, &'static LogCallsite>>,
    debug: RwLock<BTreeMap<Box<str>, &'static LogCallsite>>,
    trace: RwLock<BTreeMap<Box<str>, &'static LogCallsite>>,
}

impl LogBridge {
    /// The callsites of `level`, by target.
    const fn callsites(&self, level: Level) -> &RwLock<BTreeMap<Box<str>, &'static LogCallsite>> {
        match level {
            Level::ERROR => &self.error,
            Level::WARN => &self.warn,
            Level::INFO => &self.info,
            Level::DEBUG => &self.debug,
            Level::TRACE => &self.trace,
        }
    }

    /// The callsite of `target` at `level`, registered on first use; `None`
    /// when a lock is poisoned, which drops the record.
    fn callsite(&self, level: Level, target: &str) -> Option<&'static LogCallsite> {
        let callsites = self.callsites(level);
        if let Some(found) = callsites.read().ok()?.get(target) {
            return Some(found);
        }
        let mut callsites = callsites.write().ok()?;
        if let Some(found) = callsites.get(target) {
            return Some(found);
        }
        let callsite = LogCallsite::register(level, target)?;
        callsites.insert(target.into(), callsite);
        Some(callsite)
    }
}

impl log::Log for LogBridge {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        LevelFilter::current() >= level_of(metadata.level())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Some(callsite) = self.callsite(level_of(record.level()), record.target()) else {
            return;
        };
        let interest = callsite.interest();
        if interest.is_never() {
            return;
        }
        let Some(metadata) = callsite.metadata.get() else {
            return;
        };
        let fields = metadata.fields();
        let Some(message) = fields.field("message") else {
            return;
        };
        let args: &dyn Value = record.args();
        dispatcher::get_default(|dispatch| {
            if interest.is_always() || dispatch.enabled(metadata) {
                dispatch.event(&Event::new(
                    metadata,
                    &fields.value_set(&[(&message, Some(args))]),
                ));
            }
        });
    }

    fn flush(&self) {}
}

/// The `tracing` level of a `log` level.
const fn level_of(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::ERROR,
        log::Level::Warn => Level::WARN,
        log::Level::Info => Level::INFO,
        log::Level::Debug => Level::DEBUG,
        log::Level::Trace => Level::TRACE,
    }
}

/// The registered callsite of one target at one level, which lives as long
/// as the process: a program has a fixed set of `log` targets.
#[derive(Debug)]
struct LogCallsite {
    interest: AtomicU8,
    metadata: OnceLock<Metadata<'static>>,
}

impl LogCallsite {
    /// Leaks and registers the callsite of `target` at `level`; `None` only
    /// if its metadata was already set, which a fresh callsite never has.
    fn register(level: Level, target: &str) -> Option<&'static Self> {
        let callsite: &'static Self = Box::leak(Box::new(Self {
            interest: AtomicU8::new(UNREGISTERED),
            metadata: OnceLock::new(),
        }));
        let target: &'static str = Box::leak(target.into());
        callsite
            .metadata
            .set(Metadata::new(
                "log record",
                target,
                level,
                None,
                None,
                None,
                FieldSet::new(FIELDS, tracing_core::identify_callsite!(callsite)),
                Kind::EVENT,
            ))
            .is_ok()
            .then(|| {
                callsite::register(callsite);
                callsite
            })
    }

    /// The interest the registry set; a callsite the registry has not yet
    /// asked is asked per event.
    fn interest(&self) -> Interest {
        match self.interest.load(Ordering::Acquire) {
            NEVER => Interest::never(),
            ALWAYS => Interest::always(),
            _ => Interest::sometimes(),
        }
    }
}

impl Callsite for LogCallsite {
    fn set_interest(&self, interest: Interest) {
        let code = if interest.is_never() {
            NEVER
        } else if interest.is_always() {
            ALWAYS
        } else {
            SOMETIMES
        };
        self.interest.store(code, Ordering::Release);
    }

    #[expect(
        clippy::expect_used,
        reason = "register sets the metadata before it hands the callsite to the registry, \
                  the only caller of this method"
    )]
    fn metadata(&self) -> &Metadata<'_> {
        self.metadata
            .get()
            .expect("the callsite's metadata should be set before it is registered")
    }
}
