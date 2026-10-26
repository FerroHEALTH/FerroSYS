// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The `healthcheck` subcommand: asks the server running beside it whether
//! it is ready, for a container image with no shell and no HTTP client.
//!
//! It sends `GET {base}/health/readiness` over loopback with a short
//! timeout and exits `0` only on `200`, which a Docker `HEALTHCHECK` reads
//! as healthy (<https://docs.docker.com/reference/dockerfile/#healthcheck>).
//! It reports one line and never the body.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::health::READINESS;

/// The longest status line the check reads.
const STATUS_LINE_LIMIT: u64 = 1024;

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
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// Asks the server at `address` for `{base_path}/health/readiness`.
pub async fn check(address: SocketAddr, base_path: &str, timeout: Duration) -> Outcome {
    tokio::time::timeout(timeout, ask(address, base_path))
        .await
        .unwrap_or(Outcome::Timeout)
}

async fn ask(address: SocketAddr, base_path: &str) -> Outcome {
    let mut stream = match tokio::net::TcpStream::connect(address).await {
        Ok(stream) => stream,
        Err(error) => return Outcome::Failed(format!("cannot connect to {address}: {error}")),
    };
    let request = format!(
        "GET {base_path}{READINESS} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n"
    );
    if let Err(error) = stream.write_all(request.as_bytes()).await {
        return Outcome::Failed(format!("cannot send the request: {error}"));
    }
    let mut head = Vec::new();
    if let Err(error) = (&mut stream)
        .take(STATUS_LINE_LIMIT)
        .read_to_end(&mut head)
        .await
    {
        return Outcome::Failed(format!("cannot read the answer: {error}"));
    }
    match status(&head) {
        Some(200) => Outcome::Healthy,
        Some(code) => Outcome::Unhealthy(code),
        None => Outcome::Failed("the answer is no HTTP status line".to_owned()),
    }
}

/// The status code of the HTTP/1.1 status line `head` starts with.
fn status(head: &[u8]) -> Option<u16> {
    let line = head.split(|byte| *byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use super::{Outcome, check, status, target};

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

    #[test]
    fn the_status_is_read_from_the_status_line() {
        assert_eq!(
            Some(200),
            status(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
        );
        assert_eq!(Some(503), status(b"HTTP/1.1 503 Service Unavailable\r\n"));
        assert_eq!(None, status(b"SSH-2.0-OpenSSH\r\n"));
        assert_eq!(None, status(b""));
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
