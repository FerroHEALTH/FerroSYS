// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The panic hook: one fixed log line per panic, and never the message.
//!
//! Rust's default hook prints the panic message to stderr, and a message can
//! quote whatever value the thread held, a patient identifier or a secret
//! included. The hook installed here writes one `error` event instead,
//! naming the thread, the source location and, when the panicking thread is
//! polling a request the request log (feature `axum`) wraps, the server's id
//! of that request, and leaving the message out. Nothing a client wrote
//! reaches the line: the id is the server's own.
//! The release profile keeps `panic = "unwind"`, so the HTTP stack still
//! turns the panic into a `500`
//! (<https://doc.rust-lang.org/cargo/reference/profiles.html#panic>).

use std::cell::RefCell;
use std::panic::PanicHookInfo;
use std::sync::Arc;

/// The `tracing` target of the panic line.
pub const TARGET: &str = "ferrohealth_telemetry::panic";

// NOTE: no specification governs this: our own design; a thread-local set
// around every poll of a request names it to a hook that sees no request.
thread_local! {
    static SERVING: RefCell<Option<Arc<str>>> = const { RefCell::new(None) };
}

/// Names `request_id` as the request the current thread serves until the
/// returned guard drops, which restores the request named before.
#[cfg(feature = "axum")]
pub(crate) fn serving(request_id: Option<Arc<str>>) -> Serving {
    // NOTE: no specification governs this: our own design; a thread being torn down
    // has no slot to set, so its panics name no request.
    let previous = SERVING
        .try_with(|slot| slot.replace(request_id))
        .ok()
        .flatten();
    Serving { previous }
}

/// Restores the request named before [`serving`] when it drops, a panic
/// unwinding through the poll included.
#[cfg(feature = "axum")]
pub(crate) struct Serving {
    previous: Option<Arc<str>>,
}

#[cfg(feature = "axum")]
impl Drop for Serving {
    fn drop(&mut self) {
        let previous = self.previous.take();
        // NOTE: no specification governs this: our own design; a thread being torn
        // down has no slot left to restore, and no request left to name.
        let _restored = SERVING.try_with(|slot| slot.replace(previous));
    }
}

/// The server's id of the request the current thread serves, if any.
fn request_id() -> Option<Arc<str>> {
    // NOTE: no specification governs this: our own design; outside a served request,
    // or with the slot borrowed or gone, there is no id to name, so the line has none.
    SERVING
        .try_with(|slot| slot.try_borrow().ok().and_then(|id| id.clone()))
        .ok()
        .flatten()
}

/// Held by every test that installs the hook, which is one per process.
#[cfg(test)]
pub(crate) static HOOK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Replaces the process's panic hook with [`record`].
///
/// Call it once at start, after the subscriber is installed; the previous
/// hook, Rust's default included, is dropped.
pub fn install_hook() {
    std::panic::set_hook(Box::new(record));
}

/// Writes the panic line for `info`: the thread, the file and the line, the
/// server's `request_id` when the thread is serving a request the request
/// log wraps, and no part of the message.
///
/// A request with no `request_log::RequestId`, and a
/// panic outside any request, leave `request_id` off the line.
pub fn record(info: &PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    let request_id = request_id();
    let request_id = request_id.as_deref();
    match info.location() {
        Some(location) => tracing::error!(
            target: TARGET,
            thread,
            file = location.file(),
            line = location.line(),
            request_id,
            "a thread panicked; the message is withheld"
        ),
        None => {
            tracing::error!(
                target: TARGET,
                thread,
                request_id,
                "a thread panicked; the message is withheld"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::install_hook;
    use crate::console::layer;
    use crate::section::Rendering;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("the buffer").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn a_panic_leaves_one_line_without_its_message() {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(Rendering::Json, false, capture.clone()));
        let _hook = super::HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        install_hook();
        let outcome = tracing::subscriber::with_default(subscriber, || {
            std::panic::catch_unwind(|| {
                panic!("patient Qz7sentinel has no EHR");
            })
        });
        assert!(outcome.is_err(), "the closure panicked");
        let text = String::from_utf8(capture.0.lock().expect("the buffer").clone()).expect("UTF-8");
        assert!(
            !text.contains("Qz7sentinel"),
            "the message is withheld: {text}"
        );
        let line: serde_json::Value = serde_json::from_str(text.trim_end()).expect("one JSON line");
        assert_eq!(
            "a thread panicked; the message is withheld",
            line["message"]
        );
        assert_eq!("ERROR", line["level"]);
        assert!(
            line.get("request_id").is_none(),
            "a panic outside a request names none: {text}"
        );
        assert!(
            line["file"]
                .as_str()
                .is_some_and(|file| file.ends_with("panic.rs")),
            "{text}"
        );
        let _default = std::panic::take_hook();
    }
}
