// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The console: the `fmt` layer for a rendering, and the writer that keeps
//! a record on one line.

use std::borrow::Cow;
use std::io::{self, Write};

use tracing::{Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::registry::LookupSpan;

use crate::section::Rendering;

/// A boxed layer over the subscriber `S`.
pub type BoxedLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Returns the console layer for `rendering`, writing through `writer`.
///
/// `json` writes one object per line with the event's fields flattened and
/// no span list, so a span's fields reach the line only when the event
/// carries them. `pretty` writes colour when `ansi` holds, and every record
/// through [`LineSafe`], so a value holding a line break cannot forge a
/// second record.
pub fn layer<S, W>(rendering: Rendering, ansi: bool, writer: W) -> BoxedLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    match rendering {
        Rendering::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(false)
                .with_span_list(false)
                .with_writer(writer),
        ),
        Rendering::Pretty => Box::new(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(ansi)
                .with_writer(LineSafe::new(writer)),
        ),
    }
}

/// A [`MakeWriter`] whose writers keep each record on one line.
#[derive(Debug, Clone)]
pub struct LineSafe<M>(M);

impl<M> LineSafe<M> {
    /// Wraps `inner`.
    #[must_use]
    pub const fn new(inner: M) -> Self {
        Self(inner)
    }
}

impl<'a, M> MakeWriter<'a> for LineSafe<M>
where
    M: MakeWriter<'a>,
{
    type Writer = LineSafeWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        LineSafeWriter(self.0.make_writer())
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        LineSafeWriter(self.0.make_writer_for(meta))
    }
}

/// A writer that escapes every line break inside a record as `\n` or `\r`
/// and keeps the one that ends it.
#[derive(Debug)]
pub struct LineSafeWriter<W>(W);

// NOTE: no specification governs this: our own design. The `fmt` layer writes
// one whole record per call, so the one line feed a record may end in is its own.
impl<W> Write for LineSafeWriter<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(&one_line(buf))?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.0.write_all(&one_line(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// Returns `record` with every interior line break escaped.
fn one_line(record: &[u8]) -> Cow<'_, [u8]> {
    let (body, terminator): (&[u8], &[u8]) = match record.split_last() {
        Some((b'\n', body)) => (body, b"\n"),
        _ => (record, b""),
    };
    if !body.iter().any(|byte| matches!(byte, b'\n' | b'\r')) {
        return Cow::Borrowed(record);
    }
    let mut escaped = Vec::with_capacity(record.len() + 8);
    for byte in body {
        match byte {
            b'\n' => escaped.extend_from_slice(br"\n"),
            b'\r' => escaped.extend_from_slice(br"\r"),
            other => escaped.push(*other),
        }
    }
    escaped.extend_from_slice(terminator);
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::io::Write as _;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{LineSafeWriter, layer, one_line};
    use crate::section::Rendering;

    /// A writer that keeps what it is given.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("the buffer").clone()).expect("UTF-8")
        }
    }

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
    fn an_interior_break_is_escaped_and_the_terminator_is_kept() {
        assert_eq!(
            b"a\\r\\nb\\nc\n".to_vec(),
            one_line(b"a\r\nb\nc\n").into_owned()
        );
        assert!(matches!(one_line(b"plain\n"), Cow::Borrowed(_)));
        assert_eq!(b"no\\nend".to_vec(), one_line(b"no\nend").into_owned());
        let mut out = Vec::new();
        LineSafeWriter(&mut out)
            .write_all(b"x\ny\n")
            .expect("the write lands");
        assert_eq!(b"x\\ny\n".to_vec(), out);
    }

    #[test]
    fn json_writes_one_object_per_line_with_the_fields_flat() {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(Rendering::Json, false, capture.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(route = "/fhir/Patient/{id}", status = 200_u16, "request");
        });
        let text = capture.text();
        let line: serde_json::Value =
            serde_json::from_str(text.trim_end()).expect("one JSON object");
        assert_eq!("/fhir/Patient/{id}", line["route"]);
        assert_eq!(200, line["status"]);
        assert_eq!("request", line["message"]);
        assert_eq!(1, text.lines().count(), "{text}");
    }

    #[test]
    fn pretty_keeps_a_forged_line_break_inside_its_record() {
        let capture = Capture::default();
        let subscriber =
            tracing_subscriber::registry().with(layer(Rendering::Pretty, false, capture.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(value = "ok\nINFO forged record", "request");
        });
        assert_eq!(1, capture.text().lines().count(), "{}", capture.text());
    }
}
