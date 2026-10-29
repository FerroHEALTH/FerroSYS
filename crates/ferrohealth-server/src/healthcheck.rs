// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `healthcheck` subcommand: asks the server running beside it whether
//! it is ready, for a container image with no shell and no HTTP client.
//!
//! It sends `GET {base}/health/readiness` over loopback with a short
//! timeout and exits `0` only on `200`, which a Docker `HEALTHCHECK` reads
//! as healthy (<https://docs.docker.com/reference/dockerfile/#healthcheck>).
//! It reports one line and never the body.
//!
//! [`Get`] is the same exchange for a product that reads more than
//! readiness from its own listener: it answers the status and the body,
//! read up to a bound and within a timeout.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use http::header::{CONNECTION, HOST};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use hyper::body::Body as _;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::health::READINESS;
use crate::tls::HealthcheckTls;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The server answered `200`.
    Healthy,
    /// The server answered another status.
    Unhealthy(u16),
    /// The server did not answer inside the timeout.
    Timeout,
    /// The connection failed or the answer is no HTTP status line.
    Failed(String),
}

impl Outcome {
    /// The process exit code: `0` when healthy, `1` otherwise.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Healthy => 0,
            Self::Unhealthy(_) | Self::Timeout | Self::Failed(_) => 1,
        }
    }

    /// The outcome of an exchange whose body the check never reads: the
    /// status decides whenever one arrived.
    fn of(result: Result<Answer, GetError>) -> Self {
        let status = match result {
            Ok(answer) => answer.status,
            Err(GetError::TooLong { status, .. } | GetError::Body { status, .. }) => status,
            Err(GetError::Timeout(_)) => return Self::Timeout,
            Err(GetError::Exchange(error)) if error.is_parse() => {
                return Self::Failed("the answer is no HTTP status line".to_owned());
            }
            Err(error) => return Self::Failed(reason(&error)),
        };
        if status == StatusCode::OK {
            Self::Healthy
        } else {
            Self::Unhealthy(status.as_u16())
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => f.write_str("healthy"),
            Self::Unhealthy(status) => write!(f, "unhealthy: readiness answered {status}"),
            Self::Timeout => f.write_str("unhealthy: readiness did not answer in time"),
            Self::Failed(reason) => write!(f, "unhealthy: {reason}"),
        }
    }
}

/// The address the check connects to for a listener bound to `listen`: an
/// unspecified address (`0.0.0.0`, `::`) becomes the loopback of its family.
#[must_use]
pub fn target(listen: SocketAddr) -> SocketAddr {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, listen.port())
}

/// Asks the server at `address` for `{base_path}/health/readiness` over
/// plain HTTP.
pub async fn check(address: SocketAddr, base_path: &str, timeout: Duration) -> Outcome {
    Outcome::of(
        Get::new(address, format!("{base_path}{READINESS}"), timeout, 0)
            .send()
            .await,
    )
}

/// Asks the server at `address` for `{base_path}/health/readiness` over
/// TLS, accepting only the certificate `tls` pins.
pub async fn check_tls(
    address: SocketAddr,
    base_path: &str,
    timeout: Duration,
    tls: &HealthcheckTls,
) -> Outcome {
    Outcome::of(
        Get::new(address, format!("{base_path}{READINESS}"), timeout, 0)
            .with_tls(tls.clone())
            .send()
            .await,
    )
}

/// Asks the readiness URL `url`, `http://` or `https://`; an `https://`
/// URL needs `tls`, the certificate the listener's file holds.
pub async fn check_url(url: &str, timeout: Duration, tls: Option<&HealthcheckTls>) -> Outcome {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return Outcome::Failed(format!("{url} is no URL"));
    };
    let secure = match uri.scheme_str() {
        Some("http") => false,
        Some("https") => true,
        _ => return Outcome::Failed(format!("{url} is no http or https URL")),
    };
    let (Some(host), Some(authority)) = (
        uri.host().map(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned()
        }),
        uri.authority(),
    ) else {
        return Outcome::Failed(format!("{url} names no host"));
    };
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let target = uri
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    let request = Request {
        host: authority.as_str(),
        path: target,
        headers: &HeaderMap::new(),
        limit: 0,
    };
    let exchange = async {
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|source| GetError::Connect {
                address: format!("{host}:{port}"),
                source,
            })?;
        if !secure {
            return request.over(stream).await;
        }
        let Some(tls) = tls else {
            return Err(GetError::NoPin);
        };
        let Ok(name) = ServerName::try_from(host.clone()) else {
            return Err(GetError::ServerName(host.clone()));
        };
        request.over(handshake(stream, name, tls).await?).await
    };
    Outcome::of(
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(GetError::Timeout(timeout))),
    )
}

/// A `GET` to a server on this host, over plain HTTP or over the
/// healthcheck's pinned TLS, that reads the status and the body.
///
/// It is the exchange [`check`] and [`check_tls`] make, for a product that
/// reads more than readiness from its own listener. It speaks HTTP/1.1
/// (RFC 9112) with `connection: close`, consults no proxy and follows no
/// redirect: the server is on this host, and only its own answer counts.
///
/// ```no_run
/// # async fn read(tls: ferrohealth_server::tls::HealthcheckTls) {
/// use std::time::Duration;
/// use ferrohealth_server::healthcheck::Get;
///
/// let answer = Get::new("127.0.0.1:8443".parse().unwrap(), "/health", Duration::from_secs(3), 65_536)
///     .with_tls(tls)
///     .send()
///     .await;
/// # }
/// ```
#[derive(Clone)]
pub struct Get {
    address: SocketAddr,
    path: String,
    timeout: Duration,
    limit: usize,
    tls: Option<HealthcheckTls>,
    headers: HeaderMap,
}

impl Get {
    /// A `GET path` to `address` over plain HTTP, the whole exchange
    /// within `timeout` and the body read up to `limit` bytes.
    #[must_use]
    pub fn new(
        address: SocketAddr,
        path: impl Into<String>,
        timeout: Duration,
        limit: usize,
    ) -> Self {
        Self {
            address,
            path: path.into(),
            timeout,
            limit,
            tls: None,
            headers: HeaderMap::new(),
        }
    }

    /// Asks over `tls`, accepting only the certificate it pins.
    #[must_use]
    pub fn with_tls(mut self, tls: HealthcheckTls) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Sends the header `name` with `value`, beside any sent before.
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.append(name, value);
        self
    }

    /// Makes the request and reads the answer.
    ///
    /// # Errors
    ///
    /// Returns [`GetError::Connect`] when no connection is made,
    /// [`GetError::Handshake`] when the TLS handshake fails (a listener
    /// that presents a certificate other than the pinned one among them),
    /// [`GetError::Timeout`] when the exchange outlasts the timeout,
    /// [`GetError::TooLong`] for a body past the limit,
    /// [`GetError::Body`] when the body breaks off, and
    /// [`GetError::Exchange`] when the request fails or the answer is no
    /// HTTP.
    pub async fn send(&self) -> Result<Answer, GetError> {
        let host = self.address.to_string();
        let request = Request {
            host: &host,
            path: &self.path,
            headers: &self.headers,
            limit: self.limit,
        };
        let exchange = async {
            let stream =
                TcpStream::connect(self.address)
                    .await
                    .map_err(|source| GetError::Connect {
                        address: host.clone(),
                        source,
                    })?;
            match &self.tls {
                None => request.over(stream).await,
                Some(tls) => {
                    let name = ServerName::IpAddress(self.address.ip().into());
                    request.over(handshake(stream, name, tls).await?).await
                }
            }
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .unwrap_or(Err(GetError::Timeout(self.timeout)))
    }
}

impl fmt::Debug for Get {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NOTE: no specification governs this: our own design; a header value can be a
        // credential, so only the names are rendered.
        f.debug_struct("Get")
            .field("address", &self.address)
            .field("path", &self.path)
            .field("timeout", &self.timeout)
            .field("limit", &self.limit)
            .field("tls", &self.tls.is_some())
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The status and the body a [`Get`] read.
#[derive(Clone, PartialEq, Eq)]
pub struct Answer {
    status: StatusCode,
    body: Vec<u8>,
}

impl Answer {
    /// The status.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body, taken.
    #[must_use]
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

impl fmt::Debug for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Answer")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Why a [`Get`] read no answer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GetError {
    /// No connection could be made.
    #[error("cannot connect to {address}")]
    Connect {
        /// The address asked.
        address: String,
        /// What the connection reported.
        #[source]
        source: std::io::Error,
    },
    /// The TLS handshake failed, a certificate other than the pinned one
    /// among the causes.
    #[error("the TLS handshake failed")]
    Handshake(#[source] std::io::Error),
    /// The exchange outlasted the timeout.
    #[error("no answer within {} ms", .0.as_millis())]
    Timeout(Duration),
    /// The body is longer than the limit.
    #[error("the answer {status} has a body of more than {limit} bytes")]
    TooLong {
        /// The status the answer carried.
        status: StatusCode,
        /// The limit.
        limit: usize,
    },
    /// The body broke off.
    #[error("the body of the answer {status} broke off")]
    Body {
        /// The status the answer carried.
        status: StatusCode,
        /// What the connection reported.
        #[source]
        source: hyper::Error,
    },
    /// The request could not be sent, or the answer is no HTTP.
    #[error("the exchange failed")]
    Exchange(#[source] hyper::Error),
    /// The path or a header makes no request.
    #[error("the path or a header makes no HTTP request")]
    Request(#[source] http::Error),
    /// An `https` URL was asked with no certificate to pin.
    #[error("an https URL needs the listener's certificate to pin")]
    NoPin,
    /// The host of an `https` URL is no TLS server name.
    #[error("{0} is no TLS server name")]
    ServerName(String),
}

impl GetError {
    /// The status of the answer, when one arrived before the failure.
    #[must_use]
    pub const fn status(&self) -> Option<StatusCode> {
        match self {
            Self::TooLong { status, .. } | Self::Body { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// `error` and each of its causes, joined by `: `.
fn reason(error: &GetError) -> String {
    let mut text = error.to_string();
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        text.push_str(": ");
        text.push_str(&current.to_string());
        cause = current.source();
    }
    text
}

/// The TLS client stream over `stream`, accepting only what `tls` pins.
async fn handshake(
    stream: TcpStream,
    name: ServerName<'static>,
    tls: &HealthcheckTls,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, GetError> {
    tokio_rustls::TlsConnector::from(tls.config())
        .connect(name, stream)
        .await
        .map_err(GetError::Handshake)
}

/// One request, ready to be made over a connected stream.
struct Request<'a> {
    host: &'a str,
    path: &'a str,
    headers: &'a HeaderMap,
    limit: usize,
}

impl Request<'_> {
    /// Makes the request over `stream` and reads the answer.
    async fn over<S>(&self, stream: S) -> Result<Answer, GetError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut request = http::Request::get(self.path)
            .header(HOST, self.host)
            .header(CONNECTION, "close")
            .body(axum::body::Body::empty())
            .map_err(GetError::Request)?;
        for (name, value) in self.headers {
            request.headers_mut().append(name, value.clone());
        }
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(GetError::Exchange)?;
        let limit = self.limit;
        let mut exchange = std::pin::pin!(async move {
            let response = sender
                .send_request(request)
                .await
                .map_err(GetError::Exchange)?;
            let status = response.status();
            let mut body = response.into_body();
            let mut read = Vec::new();
            while let Some(frame) =
                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await
            {
                let frame = frame.map_err(|source| GetError::Body { status, source })?;
                // NOTE: RFC 9112 §7.1.2; a trailer section is no part of the body.
                if let Ok(data) = frame.into_data() {
                    if read.len().saturating_add(data.len()) > limit {
                        return Err(GetError::TooLong { status, limit });
                    }
                    read.extend_from_slice(&data);
                }
            }
            Ok(Answer { status, body: read })
        });
        let mut connection = std::pin::pin!(connection);
        // NOTE: no specification governs this: our own design; the connection is driven
        // until the answer is read, and a connection that ends first leaves its error there.
        tokio::select! {
            biased;
            answer = &mut exchange => answer,
            _ended = &mut connection => exchange.await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{Get, GetError, Outcome, check, target};

    #[test]
    fn an_unspecified_listener_is_asked_on_loopback() {
        assert_eq!(
            "127.0.0.1:8080",
            target("0.0.0.0:8080".parse::<SocketAddr>().expect("v4")).to_string()
        );
        assert_eq!(
            "[::1]:8080",
            target("[::]:8080".parse::<SocketAddr>().expect("v6")).to_string()
        );
        assert_eq!(
            "10.0.0.5:8080",
            target("10.0.0.5:8080".parse::<SocketAddr>().expect("v4")).to_string()
        );
    }

    /// The outcome of `check` against a server that answers `answer`
    /// byte for byte and closes.
    async fn against(answer: &'static [u8]) -> Outcome {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let address = listener.local_addr().expect("the address");
        tokio::spawn(async move {
            let (mut stream, _peer) = listener.accept().await.expect("a connection");
            let mut head = [0_u8; 1024];
            let _read = stream.read(&mut head).await;
            stream
                .write_all(answer)
                .await
                .expect("the answer is written");
        });
        check(address, "", Duration::from_secs(5)).await
    }

    #[tokio::test]
    async fn the_status_is_read_from_the_status_line() {
        assert_eq!(
            Outcome::Healthy,
            against(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await
        );
        assert_eq!(
            Outcome::Unhealthy(503),
            against(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n").await
        );
        assert!(matches!(
            against(b"SSH-2.0-OpenSSH\r\n").await,
            Outcome::Failed(_)
        ));
        assert!(matches!(against(b"").await, Outcome::Failed(_)));
    }

    #[tokio::test]
    async fn a_body_is_read_with_the_headers_sent_and_refused_past_its_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let address = listener.local_addr().expect("the address");
        tokio::spawn(async move {
            loop {
                let (mut stream, _peer) = listener.accept().await.expect("a connection");
                let mut head = vec![0_u8; 4096];
                let read = stream.read(&mut head).await.expect("the head");
                let head = String::from_utf8_lossy(head.get(..read).expect("in bounds"));
                let token = head
                    .lines()
                    .find_map(|line| line.strip_prefix("authorization: "))
                    .unwrap_or("none")
                    .to_owned();
                let answer = format!(
                    "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{token}\r\n0\r\n\r\n",
                    token.len()
                );
                stream.write_all(answer.as_bytes()).await.expect("written");
            }
        });
        let answer = Get::new(address, "/x", Duration::from_secs(5), 64)
            .with_header(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_static("Bearer t0ken"),
            )
            .send()
            .await
            .expect("an answer");
        assert_eq!(http::StatusCode::OK, answer.status());
        assert_eq!(b"Bearer t0ken", answer.body());
        let error = Get::new(address, "/x", Duration::from_secs(5), 3)
            .send()
            .await
            .expect_err("four bytes are past a bound of three");
        assert!(
            matches!(error, GetError::TooLong { limit: 3, .. }),
            "{error:?}"
        );
        assert_eq!(Some(http::StatusCode::OK), error.status());
    }

    #[test]
    fn a_header_value_never_reaches_the_debug_rendering() {
        let get = Get::new(
            "127.0.0.1:1".parse().expect("an address"),
            "/",
            Duration::from_secs(1),
            0,
        )
        .with_header(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_static("Bearer CANARY"),
        );
        let rendered = format!("{get:?}");
        assert!(rendered.contains("authorization"), "{rendered}");
        assert!(!rendered.contains("CANARY"), "{rendered}");
    }

    #[tokio::test]
    async fn a_closed_port_fails_and_the_exit_code_says_so() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("the address");
        drop(listener);
        let outcome = check(address, "", Duration::from_secs(2)).await;
        assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
        assert_eq!(1, outcome.exit_code());
        assert_eq!(0, Outcome::Healthy.exit_code());
        assert_eq!(
            "unhealthy: readiness answered 503",
            Outcome::Unhealthy(503).to_string()
        );
    }
}
