// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! HL7 v2 messages sent over MLLP as load, beside the HTTP requests.
//!
//! The plan names the messages under `[[messages]]`, each a file holding one
//! HL7 v2 message, and the listener under `[mllp]`. Each send frames the
//! message by the Minimal Lower Layer Protocol, Release 1 (HL7 v2.5.1
//! Implementation Guide: Transport Specification, MLLP Release 1: a start
//! block `0x0B`, the message, an end block `0x1C` and a carriage return
//! `0x0D`) and waits for the acknowledgment on the same connection. MSA-1
//! `AA` or `CA` answers it; any other code (`AE`, `AR`, `CE`, `CR`) counts as
//! a failed request. A receiver keeps the acknowledgment of each MSH-10
//! (HL7 v2.5.1 §2.10.1, the message control id it echoes in MSA-2), so every
//! send carries a fresh MSH-10 of 20 characters, the length v2.3.1 allows,
//! and the acknowledgment must echo it. Connections are kept and reused one
//! message at a time; a kept connection the receiver closed is replaced
//! once. TLS takes the plan's client identity and trust roots, as the HTTP
//! requests do.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::plan::Plan;

/// The start block of an MLLP frame.
const START: u8 = 0x0B;

/// The end block of an MLLP frame, followed by [`CR`].
const END: u8 = 0x1C;

/// The carriage return that ends a frame and separates segments.
const CR: u8 = 0x0D;

/// The longest acknowledgment read before the answer counts as malformed.
const MAX_ACK: usize = 1 << 20;

/// A message file or an MLLP setting that cannot be used.
#[derive(Debug, Error)]
pub enum MllpError {
    /// The message file could not be read.
    #[error("cannot read the HL7 v2 message {path}")]
    Read {
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The file holds no HL7 v2 message whose MSH-10 can be set.
    #[error("the HL7 v2 message {path} is refused: {reason}")]
    Message {
        /// The file.
        path: String,
        /// Why.
        reason: &'static str,
    },
    /// The plan sends messages and names no listener.
    #[error(
        "the plan sends HL7 v2 messages and names no MLLP listener: set [mllp] address or give one on the command line"
    )]
    NoAddress,
    /// The TLS settings could not be built.
    #[error("the MLLP TLS settings cannot be built: {0}")]
    Tls(&'static str, #[source] Option<rustls::Error>),
    /// A credential file of the plan could not be used.
    #[error(transparent)]
    Credential(#[from] crate::credentials::CredentialError),
}

/// The bytes of the message file at `path`.
fn raw(path: &Path) -> Result<Vec<u8>, MllpError> {
    std::fs::read(path).map_err(|source| MllpError::Read {
        path: path.display().to_string(),
        source,
    })
}

/// The refusal of the message file at `path`.
fn refused(path: &Path, reason: &'static str) -> MllpError {
    MllpError::Message {
        path: path.display().to_string(),
        reason,
    }
}

/// One HL7 v2 message of the plan, split around its MSH-10.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    before: Vec<u8>,
    after: Vec<u8>,
}

impl Message {
    /// Reads the message at `path`.
    ///
    /// # Errors
    ///
    /// [`MllpError::Read`] and every error of [`Message::parse`].
    pub fn read(path: &Path) -> Result<Self, MllpError> {
        let bytes = raw(path)?;
        Self::parse(&bytes).map_err(|reason| refused(path, reason))
    }

    /// Reads the message at `path` with every placeholder filled from
    /// `values` ([`crate::values`]).
    ///
    /// # Errors
    ///
    /// The errors of [`Message::read`], and [`MllpError::Message`] for a
    /// placeholder with no value.
    pub fn read_filled(path: &Path, values: &crate::values::Values) -> Result<Self, MllpError> {
        let filled = values
            .fill_bytes(&raw(path)?)
            .map_err(|_missing| refused(path, "a placeholder of it has no value"))?;
        Self::parse(&filled).map_err(|reason| refused(path, reason))
    }

    /// Parses one message. Line feeds and carriage-return line feeds end a
    /// segment as a carriage return does, and the message ends with one.
    ///
    /// # Errors
    ///
    /// The reason, when the text does not open with an MSH segment that
    /// holds an MSH-10 field.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let mut text = Vec::with_capacity(bytes.len().saturating_add(1));
        let mut rest = bytes.iter().copied().peekable();
        while let Some(byte) = rest.next() {
            match byte {
                CR if rest.peek() == Some(&b'\n') => {}
                b'\n' => text.push(CR),
                other => text.push(other),
            }
        }
        while text.last().is_some_and(u8::is_ascii_whitespace) {
            text.pop();
        }
        text.push(CR);
        if !text.starts_with(b"MSH") {
            return Err("it does not open with an MSH segment");
        }
        let separator = text
            .get(3)
            .copied()
            .filter(|byte| *byte != CR)
            .ok_or("its MSH segment is cut short")?;
        let header_end = text.iter().position(|b| *b == CR).unwrap_or(text.len());
        // NOTE: HL7 v2.5.1 §2.14.9: MSH-1 is the field separator itself, so MSH-10 follows the ninth separator.
        let start = text
            .iter()
            .take(header_end)
            .enumerate()
            .filter(|(_, byte)| **byte == separator)
            .nth(8)
            .map(|(index, _)| index.saturating_add(1))
            .ok_or("its MSH segment holds no MSH-10 field")?;
        let length = text
            .iter()
            .skip(start)
            .take(header_end.saturating_sub(start))
            .position(|b| *b == separator)
            .unwrap_or(header_end.saturating_sub(start));
        let (before, rest) = text
            .split_at_checked(start)
            .ok_or("its MSH segment is cut short")?;
        let after = rest.get(length..).ok_or("its MSH segment is cut short")?;
        Ok(Self {
            before: before.to_vec(),
            after: after.to_vec(),
        })
    }

    /// The MLLP frame of the message with `control_id` as its MSH-10.
    #[must_use]
    pub fn framed(&self, control_id: &str) -> Vec<u8> {
        let mut frame = Vec::with_capacity(
            self.before
                .len()
                .saturating_add(control_id.len())
                .saturating_add(self.after.len())
                .saturating_add(3),
        );
        frame.push(START);
        frame.extend_from_slice(&self.before);
        frame.extend_from_slice(control_id.as_bytes());
        frame.extend_from_slice(&self.after);
        frame.extend_from_slice(&[END, CR]);
        frame
    }
}

/// The verdict on an acknowledgment of the message `control_id`: `Ok` for
/// MSA-1 `AA` or `CA` echoing it in MSA-2, else the failure kind.
///
/// # Errors
///
/// The failure kind: `ack AE` and the like for another code, `ack without
/// MSA`, or `ack of another message`.
pub fn judge_ack(ack: &[u8], control_id: &str) -> Result<(), String> {
    let separator = ack
        .strip_prefix(b"MSH")
        .and_then(|rest| rest.first())
        .copied()
        .unwrap_or(b'|');
    let msa = ack
        .split(|b| *b == CR || *b == b'\n')
        .find(|segment| segment.starts_with(b"MSA"))
        .ok_or_else(|| "ack without MSA".to_owned())?;
    let mut fields = msa.split(|b| *b == separator).skip(1);
    let code = fields.next().unwrap_or_default();
    let echoed = fields.next().unwrap_or_default();
    match code {
        b"AA" | b"CA" if echoed == control_id.as_bytes() => Ok(()),
        b"AA" | b"CA" => Err("ack of another message".to_owned()),
        other => Err(format!("ack {}", String::from_utf8_lossy(other))),
    }
}

/// One connection to the listener.
enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Plain(_) => "Stream::Plain",
            Self::Tls(_) => "Stream::Tls",
        })
    }
}

impl Stream {
    async fn exchange(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        match self {
            Self::Plain(stream) => exchange(stream, frame).await,
            Self::Tls(stream) => exchange(stream.as_mut(), frame).await,
        }
    }
}

/// Writes `frame` and reads one framed answer, its blocks stripped.
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    frame: &[u8],
) -> Result<Vec<u8>, String> {
    let transport = |_failed: std::io::Error| "transport".to_owned();
    stream.write_all(frame).await.map_err(transport)?;
    stream.flush().await.map_err(transport)?;
    let mut answer = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await.map_err(transport)?;
        if read == 0 {
            return Err("transport".to_owned());
        }
        answer.extend_from_slice(buffer.get(..read).unwrap_or_default());
        if let Some(end) = answer.windows(2).position(|pair| pair == [END, CR]) {
            answer.truncate(end);
            let body = answer
                .strip_prefix(&[START])
                .ok_or_else(|| "ack frame".to_owned())?;
            return Ok(body.to_vec());
        }
        if answer.len() > MAX_ACK {
            return Err("ack frame".to_owned());
        }
    }
}

/// Sends the plan's messages to one listener, keeping its connections.
pub struct Sender {
    address: String,
    tls: Option<(TlsConnector, ServerName<'static>)>,
    messages: Vec<Arc<Message>>,
    idle: Mutex<Vec<Stream>>,
    prefix: String,
    sent: AtomicU64,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("address", &self.address)
            .field("tls", &self.tls.is_some())
            .field("messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

impl Sender {
    /// The sender of `plan`'s messages to `address`, or to the plan's
    /// `[mllp] address` when none is given; `None` when the plan sends no
    /// message.
    ///
    /// # Errors
    ///
    /// [`MllpError::NoAddress`] when the plan sends messages and names no
    /// listener, every error of [`Message::read`], and
    /// [`MllpError::Credential`] or [`MllpError::Tls`] for TLS settings that
    /// cannot be used.
    pub fn new(plan: &Plan, address: Option<&str>) -> Result<Option<Self>, MllpError> {
        Self::build(plan, address, Message::read)
    }

    /// The sender of [`Sender::new`], each message file filled with `values`
    /// as it is read.
    ///
    /// # Errors
    ///
    /// Every error of [`Sender::new`], and [`MllpError::Message`] for a
    /// placeholder of a message file with no value.
    pub fn filled(
        plan: &Plan,
        address: Option<&str>,
        values: &crate::values::Values,
    ) -> Result<Option<Self>, MllpError> {
        Self::build(plan, address, |path| Message::read_filled(path, values))
    }

    fn build(
        plan: &Plan,
        address: Option<&str>,
        read: impl Fn(&Path) -> Result<Message, MllpError>,
    ) -> Result<Option<Self>, MllpError> {
        if plan.messages.is_empty() {
            return Ok(None);
        }
        let settings = plan.mllp.clone().unwrap_or_default();
        let address = address
            .map(str::to_owned)
            .or(settings.address)
            .ok_or(MllpError::NoAddress)?;
        let tls = if settings.tls {
            let host = settings.server_name.unwrap_or_else(|| host_of(&address));
            Some(tls(plan, host)?)
        } else {
            None
        };
        let messages = plan
            .messages
            .iter()
            .map(|message| read(&message.file).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        Ok(Some(Self {
            address,
            tls,
            messages,
            idle: Mutex::new(Vec::new()),
            prefix: nonce.chars().take(8).collect(),
            sent: AtomicU64::new(0),
        }))
    }

    /// The message of the plan at `index`.
    #[must_use]
    pub fn message(&self, index: usize) -> Option<&Arc<Message>> {
        self.messages.get(index)
    }

    /// The listener's address, `host:port`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Sends `message` with a fresh MSH-10 and waits for its
    /// acknowledgment, within `timeout`.
    ///
    /// # Errors
    ///
    /// The failure kind: `timeout`, `transport`, `ack frame`, or one of
    /// [`judge_ack`].
    pub async fn send(&self, message: &Message, timeout: Duration) -> Result<(), String> {
        let count = self.sent.fetch_add(1, Ordering::Relaxed);
        let control_id = format!("{}{count:012x}", self.prefix);
        let frame = message.framed(&control_id);
        let ack = tokio::time::timeout(timeout, self.exchange(&frame))
            .await
            .map_err(|_elapsed| "timeout".to_owned())??;
        judge_ack(&ack, &control_id)
    }

    /// Exchanges `frame` on a kept connection, or a new one; a kept one that
    /// fails is replaced once, since the listener may have closed it.
    async fn exchange(&self, frame: &[u8]) -> Result<Vec<u8>, String> {
        let kept = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        // NOTE: no specification governs this: our own design; a kept connection that fails is one the listener closed, so the frame goes once more on a fresh one.
        let (stream, answer) = if let Some(mut stream) = kept
            && let Ok(answer) = stream.exchange(frame).await
        {
            (stream, answer)
        } else {
            let mut fresh = self.connect().await?;
            let answer = fresh.exchange(frame).await?;
            (fresh, answer)
        };
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(stream);
        Ok(answer)
    }

    async fn connect(&self) -> Result<Stream, String> {
        let tcp = TcpStream::connect(&self.address)
            .await
            .map_err(|_refused| "transport".to_owned())?;
        tcp.set_nodelay(true)
            .map_err(|_unsupported| "transport".to_owned())?;
        match &self.tls {
            None => Ok(Stream::Plain(tcp)),
            Some((connector, name)) => connector
                .connect(name.clone(), tcp)
                .await
                .map(|tls| Stream::Tls(Box::new(tls)))
                .map_err(|_handshake| "tls".to_owned()),
        }
    }
}

/// The host of `address`, `host:port` or `[v6]:port`.
fn host_of(address: &str) -> String {
    let host = address.rsplit_once(':').map_or(address, |(host, _)| host);
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned()
}

/// The TLS connector of `plan`'s client identity and trust roots, beside the
/// platform's, verifying the listener as `host`.
fn tls(plan: &Plan, host: String) -> Result<(TlsConnector, ServerName<'static>), MllpError> {
    let read = |path: &Path, what: &'static str| {
        std::fs::read(path).map_err(|source| crate::credentials::CredentialError::Read {
            what,
            path: path.display().to_string(),
            source,
        })
    };
    let parse = |path: &Path, what: &'static str| crate::credentials::CredentialError::Parse {
        what,
        path: path.display().to_string(),
        source: None,
    };
    let mut roots = Vec::new();
    if let Some(path) = plan.load.trust_roots_file.as_deref() {
        let pem = read(path, "trust roots")?;
        roots = CertificateDer::pem_slice_iter(&pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_bad| parse(path, "trust roots"))?;
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier =
        rustls_platform_verifier::Verifier::new_with_extra_roots(roots, Arc::clone(&provider))
            .map_err(|error| MllpError::Tls("the trust roots", Some(error)))?;
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| MllpError::Tls("the protocol versions", Some(error)))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let config = match plan.load.client_identity_file.as_deref() {
        None => builder.with_no_client_auth(),
        Some(path) => {
            let pem = read(path, "client identity")?;
            let chain = CertificateDer::pem_slice_iter(&pem)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_bad| parse(path, "client identity"))?;
            let key = PrivateKeyDer::from_pem_slice(&pem)
                .map_err(|_bad| parse(path, "client identity"))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|error| MllpError::Tls("the client identity", Some(error)))?
        }
    };
    let name =
        ServerName::try_from(host).map_err(|_invalid| MllpError::Tls("the server name", None))?;
    Ok((TlsConnector::from(Arc::new(config)), name))
}

#[cfg(test)]
mod tests;
