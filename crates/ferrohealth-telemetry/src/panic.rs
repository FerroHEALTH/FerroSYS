// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The panic hook: one fixed log line per panic, and never the message.
//!
//! Rust's default hook prints the panic message to stderr, and a message can
//! quote whatever value the thread held, a patient identifier or a secret
//! included. The hook installed here writes one `error` event instead,
//! naming the thread and the source location and leaving the message out.
//! The release profile keeps `panic = "unwind"`, so the HTTP stack still
//! turns the panic into a `500`
//! (<https://doc.rust-lang.org/cargo/reference/profiles.html#panic>).

use std::panic::PanicHookInfo;

/// The `tracing` target of the panic line.
pub const TARGET: &str = "ferrohealth_telemetry::panic";

/// Replaces the process's panic hook with [`record`].
///
/// Call it once at start, after the subscriber is installed; the previous
/// hook, Rust's default included, is dropped.
pub fn install_hook() {
    std::panic::set_hook(Box::new(record));
}

/// Writes the panic line for `info`: the thread, the file and the line, and
/// no part of the message.
pub fn record(info: &PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    match info.location() {
        Some(location) => tracing::error!(
            target: TARGET,
            thread,
            file = location.file(),
            line = location.line(),
            "a thread panicked; the message is withheld"
        ),
        None => {
            tracing::error!(target: TARGET, thread, "a thread panicked; the message is withheld");
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
            line["file"]
                .as_str()
                .is_some_and(|file| file.ends_with("panic.rs")),
            "{text}"
        );
        let _default = std::panic::take_hook();
    }
}
