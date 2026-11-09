// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The tests of serving and the drain, one child per subject, and the
//! listeners, certificates and requests every one of them shares.

mod connection;
mod drain;
mod tls;

use std::time::Duration;

use axum::Router;
use ferrohealth_health::lifecycle::Lifecycle;
use ferrohealth_health::registry::Registry;
use tokio::net::TcpListener;

use super::{Drained, serve};
use crate::health::Health;
use crate::section::ServerSection;
use crate::stack::{StackPolicy, app};

async fn start(
    product: Router,
    delay: Duration,
    bound: Duration,
) -> (
    std::net::SocketAddr,
    Lifecycle,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<Drained>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let app = app(
        product,
        Health::new(Registry::default(), lifecycle.clone()),
        &ServerSection::default(),
        StackPolicy::default(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let signal = async move {
        let _received = stopped.await;
    };
    let served = tokio::spawn(serve(
        listener,
        app,
        lifecycle.clone(),
        signal,
        delay,
        bound,
    ));
    (address, lifecycle, stop, served)
}

struct Certificate {
    _dir: tempfile::TempDir,
    section: crate::section::TlsSection,
}

fn certificate() -> Certificate {
    let generated =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("a certificate is generated");
    let dir = tempfile::tempdir().expect("a directory");
    let certificate_file = dir.path().join("server.pem");
    let key_file = dir.path().join("server.key");
    std::fs::write(&certificate_file, generated.cert.pem()).expect("the certificate is written");
    std::fs::write(&key_file, generated.signing_key.serialize_pem()).expect("the key is written");
    Certificate {
        _dir: dir,
        section: crate::section::TlsSection::new(certificate_file, key_file),
    }
}

/// The status line and body of `GET path` on `address`.
async fn fetch(address: std::net::SocketAddr, path: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("a connection");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("the request is written");
    let mut answer = String::new();
    let _read = stream.read_to_string(&mut answer).await;
    answer
}

/// Serves `product` through the stack with `section` on a fresh port
/// until the returned sender fires.
async fn start_run(
    product: Router,
    section: ServerSection,
) -> (
    std::net::SocketAddr,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<Drained, super::ServeError>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("the address");
    let lifecycle = Lifecycle::default();
    let app = app(
        product,
        Health::new(Registry::default(), lifecycle.clone()),
        &section,
        StackPolicy::default(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        super::run(listener, app, &section, lifecycle, async move {
            let _received = stopped.await;
        })
        .await
    });
    (address, stop, served)
}

fn short_head_bound() -> ServerSection {
    ServerSection {
        header_read_timeout: ferrohealth_config::units::Duration::from_millis(200),
        drain_delay: ferrohealth_config::units::Duration::ZERO,
        ..ServerSection::default()
    }
}

/// How long until `stream` is closed by the server, waiting at most five
/// seconds; `None` when it is still open then.
async fn closed_after<S>(stream: &mut S) -> Option<Duration>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let started = std::time::Instant::now();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest)).await;
    read.is_ok().then(|| started.elapsed())
}

/// Sends a `POST path` with `size` body bytes, then `GET /health/liveness`
/// on the same connection, and returns everything the server answered.
async fn post_then_get(address: std::net::SocketAddr, path: &str, size: usize) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connects");
    let (mut reader, mut writer) = stream.into_split();
    let head = format!(
        "POST {path} HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/xml\r\ncontent-length: {size}\r\n\r\n"
    );
    let sent = tokio::spawn(async move {
        writer.write_all(head.as_bytes()).await?;
        writer.write_all(&vec![b'x'; size]).await?;
        writer
            .write_all(
                b"GET /health/liveness HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
            )
            .await?;
        Ok::<_, std::io::Error>(writer)
    });
    let mut answer = Vec::new();
    let _read = tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut answer))
        .await
        .expect("the server answers or closes");
    let _sent = sent.await;
    String::from_utf8_lossy(&answer).to_ascii_lowercase()
}
