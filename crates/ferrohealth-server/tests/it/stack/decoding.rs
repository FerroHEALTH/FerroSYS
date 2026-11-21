// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The request decoding: a gzip or Brotli body reaches the handler decoded,
//! another coding is refused `415`, a body that decodes past the limit
//! `413` and a corrupt stream `400`, and off it changes nothing.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::routing::post;
use ferrohealth_config::units::ByteSize;
use http::{StatusCode, header};
use hyper::body::Frame;
use tower::ServiceExt as _;
use tower_http::compression::CompressionLayer;
use tower_http::compression::predicate::SizeAbove;

use ferrohealth_server::refusal;
use ferrohealth_server::section::ServerSection;
use ferrohealth_server::stack::{StackPolicy, app};

use super::{refusing, section, send, serving};

/// The decoded limit of these cases.
const LIMIT: usize = 4096;

fn echo() -> Router {
    Router::new().route("/echo", post(|body: Bytes| async move { body }))
}

fn decoding(on: bool) -> ServerSection {
    ServerSection {
        body_limit: ByteSize::from_kib(4),
        request_decompression: on,
        ..section()
    }
}

/// `plain` in `coding`, as the stack's own response compression writes it.
async fn encoded(coding: &str, plain: &[u8]) -> Bytes {
    let plain = Bytes::copy_from_slice(plain);
    let compressing = tower::ServiceBuilder::new()
        .layer(
            CompressionLayer::new()
                .gzip(true)
                .br(true)
                .compress_when(SizeAbove::new(0)),
        )
        .service_fn(move |_request: http::Request<Body>| {
            let plain = plain.clone();
            async move {
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(header::CONTENT_TYPE, "text/plain")
                        .body(Body::from(plain))
                        .expect("the response builds"),
                )
            }
        });
    let response = compressing
        .oneshot(
            http::Request::builder()
                .header(header::ACCEPT_ENCODING, coding)
                .body(Body::empty())
                .expect("the request builds"),
        )
        .await
        .expect("compressed");
    assert_eq!(
        Some(coding),
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
    );
    axum::body::to_bytes(Body::new(response.into_body()), usize::MAX)
        .await
        .expect("the compressed body")
}

fn posted(coding: Option<&str>, body: impl Into<Body>) -> http::Request<Body> {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/ferrosys/echo");
    if let Some(coding) = coding {
        request = request.header(header::CONTENT_ENCODING, coding);
    }
    request.body(body.into()).expect("the request builds")
}

async fn echoed(app: Router, request: http::Request<Body>) -> (StatusCode, http::HeaderMap, Bytes) {
    let response = app.oneshot(request).await.expect("the request is served");
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the body");
    (status, headers, body)
}

fn composition() -> Vec<u8> {
    br#"{"_type":"COMPOSITION","name":{"value":"International Patient Summary"}}"#.repeat(40)
}

#[tokio::test]
async fn a_plain_body_reaches_the_handler_as_sent() {
    let app = app(echo(), serving(), &decoding(true), StackPolicy::default());
    let plain = composition();
    let (status, _, body) = echoed(app, posted(None, plain.clone())).await;
    assert_eq!((StatusCode::OK, plain.as_slice()), (status, body.as_ref()));
}

#[tokio::test]
async fn a_gzip_body_reaches_the_handler_decoded() {
    let plain = composition();
    let gzip = encoded("gzip", &plain).await;
    assert!(
        gzip.len() * 4 < plain.len(),
        "{} of {}",
        gzip.len(),
        plain.len()
    );
    for coding in ["gzip", "GZip", "x-gzip"] {
        let app = app(echo(), serving(), &decoding(true), StackPolicy::default());
        let (status, _, body) = echoed(app, posted(Some(coding), gzip.clone())).await;
        assert_eq!(
            (StatusCode::OK, plain.as_slice()),
            (status, body.as_ref()),
            "{coding}"
        );
    }
}

#[tokio::test]
async fn a_brotli_body_reaches_the_handler_decoded() {
    let plain = composition();
    let br = encoded("br", &plain).await;
    let app = app(echo(), serving(), &decoding(true), StackPolicy::default());
    let (status, _, body) = echoed(app, posted(Some("br"), br)).await;
    assert_eq!((StatusCode::OK, plain.as_slice()), (status, body.as_ref()));
}

#[tokio::test]
async fn another_coding_is_refused_415_with_accept_encoding_in_the_product_shape() {
    for coding in ["zstd", "deflate", "gzip, br"] {
        let app = app(echo(), serving(), &decoding(true), refusing());
        let (status, headers, body) = send(app, posted(Some(coding), composition())).await;
        assert_eq!(StatusCode::UNSUPPORTED_MEDIA_TYPE, status, "{coding}");
        assert_eq!(
            Some("gzip, br"),
            headers
                .get(header::ACCEPT_ENCODING)
                .and_then(|value| value.to_str().ok()),
            "{coding}"
        );
        let refused: serde_json::Value = serde_json::from_str(&body).expect("the product's body");
        assert_eq!(415, refused["refused"], "{body}");
        assert_eq!(refusal::UNSUPPORTED_CODING, refused["reason"], "{body}");
    }
}

/// A request body sent in chunks, which counts the chunks the server read.
struct Chunks {
    chunks: VecDeque<Bytes>,
    read: Arc<AtomicUsize>,
}

impl HttpBody for Chunks {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        Poll::Ready(this.chunks.pop_front().map(|chunk| {
            this.read.fetch_add(1, Ordering::Relaxed);
            Ok(Frame::data(chunk))
        }))
    }
}

#[tokio::test]
async fn a_body_that_decodes_past_the_limit_is_refused_413_without_decoding_the_rest() {
    // A mebibyte of zeros is about a kibibyte in gzip, under the wire limit.
    let bomb = encoded("gzip", &vec![0_u8; 1 << 20]).await;
    assert!(bomb.len() < LIMIT, "{}", bomb.len());
    let chunks: VecDeque<Bytes> = bomb.chunks(16).map(Bytes::copy_from_slice).collect();
    let sent = chunks.len();
    let read = Arc::new(AtomicUsize::new(0));
    let streamed = Body::new(Chunks {
        chunks,
        read: Arc::clone(&read),
    });
    let app = app(echo(), serving(), &decoding(true), refusing());
    let (status, _, body) = send(app.clone(), posted(Some("gzip"), streamed)).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status, "{body}");
    let refused: serde_json::Value = serde_json::from_str(&body).expect("the product's body");
    assert_eq!(refusal::TOO_LARGE, refused["reason"], "{body}");
    let read = read.load(Ordering::Relaxed);
    assert!(read * 4 < sent, "read {read} of {sent} chunks");

    let (status, _, body) = send(app, posted(Some("gzip"), bomb)).await;
    assert_eq!(StatusCode::PAYLOAD_TOO_LARGE, status, "{body}");
}

#[tokio::test]
async fn a_corrupt_stream_is_refused_400_in_the_product_shape() {
    let app = app(echo(), serving(), &decoding(true), refusing());
    for coding in ["gzip", "br"] {
        let (status, _, body) = send(
            app.clone(),
            posted(Some(coding), "this was never compressed, whatever it says"),
        )
        .await;
        assert_eq!(StatusCode::BAD_REQUEST, status, "{coding}: {body}");
        let refused: serde_json::Value = serde_json::from_str(&body).expect("the product's body");
        assert_eq!(refusal::UNDECODABLE, refused["reason"], "{body}");
    }
}

#[tokio::test]
async fn off_by_default_the_body_reaches_the_handler_as_sent() {
    assert!(!ServerSection::default().request_decompression);
    let gzip = encoded("gzip", &composition()).await;
    let app = app(echo(), serving(), &decoding(false), refusing());
    let (status, _, body) = echoed(app.clone(), posted(Some("gzip"), gzip.clone())).await;
    assert_eq!((StatusCode::OK, gzip.as_ref()), (status, body.as_ref()));
    let (status, _, _) = echoed(app, posted(Some("zstd"), "plain")).await;
    assert_eq!(StatusCode::OK, status);
}
