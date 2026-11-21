// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The decoding of a compressed request body, when `[server]
//! request_decompression` is on.
//!
//! [`admit`] reads the request's `Content-Encoding` at the edge: no coding,
//! `gzip` (or `x-gzip`) and `br` pass, and any other coding, or more than
//! one, is refused `415` with `Accept-Encoding` (RFC 9110 §8.4, §15.5.16;
//! RFC 7694 §3). Inside the wire body limit, [`DecodeLayer`] decodes the
//! body with `tower-http` and caps the decoded bytes at the same limit,
//! so a small compressed body cannot expand past it: the frame that would
//! pass the limit ends the body with `http_body_util::LengthLimitError`,
//! which a body extractor answers `413`, and the rest is never decoded.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::BoxError;
use axum::body::{Body, Bytes};
use axum::response::Response;
use http::{HeaderMap, HeaderValue, Request, header};
use http_body_util::Limited;
use tower::util::{MapRequest, MapResponse};
use tower_http::body::UnsyncBoxBody;
use tower_http::decompression::{DecompressionBody, RequestDecompression};

use crate::edge::Watched;

/// The codings the server decodes, as the `Accept-Encoding` of a `415`
/// names them (RFC 7694 §3).
pub(crate) const ACCEPTED: HeaderValue = HeaderValue::from_static("gzip, br");

/// A request whose `Content-Encoding` names a coding the server does not
/// decode, or more than one.
#[derive(Debug)]
pub(crate) struct Unsupported;

/// Reads the `Content-Encoding` of a request, and returns whether its body
/// is encoded.
///
/// A coding the decoder reads is rewritten to its lower-case name, since a
/// coding is case-insensitive (RFC 9110 §8.4.1) and `x-gzip` is `gzip`
/// (RFC 9110 §8.4.1.3); an empty field names no coding and is removed.
pub(crate) fn admit(headers: &mut HeaderMap) -> Result<bool, Unsupported> {
    let mut codings = Vec::new();
    for value in headers.get_all(header::CONTENT_ENCODING) {
        let Ok(value) = value.to_str() else {
            return Err(Unsupported);
        };
        codings.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|coding| !coding.is_empty()),
        );
    }
    let coding = match codings.as_slice() {
        [] => None,
        [coding]
            if coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip") =>
        {
            Some(HeaderValue::from_static("gzip"))
        }
        [coding] if coding.eq_ignore_ascii_case("br") => Some(HeaderValue::from_static("br")),
        _ => return Err(Unsupported),
    };
    if let Some(coding) = coding {
        headers.insert(header::CONTENT_ENCODING, coding);
        Ok(true)
    } else {
        headers.remove(header::CONTENT_ENCODING);
        Ok(false)
    }
}

/// What the decoded body of one encoded request reports to the edge: the
/// limit, and the marks of a body past it and of a stream that failed.
#[derive(Clone, Debug)]
pub(crate) struct Watch {
    /// The body limit, in decoded bytes.
    pub(crate) limit: usize,
    /// Set once the decoded body passed the limit.
    pub(crate) over: Arc<AtomicBool>,
    /// Set once the stream failed to decode.
    pub(crate) failed: Arc<AtomicBool>,
}

/// The layer that decodes an encoded body the edge admitted.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DecodeLayer;

/// The service of [`DecodeLayer`]: `tower-http`'s decoder around the cap,
/// with its response body put back in an axum body.
pub(crate) type Decode<S> = MapResponse<
    RequestDecompression<MapRequest<S, fn(Request<DecompressionBody<Body>>) -> Request<Body>>>,
    fn(Response<UnsyncBoxBody<Bytes, BoxError>>) -> Response,
>;

impl<S> tower::Layer<S> for DecodeLayer {
    type Service = Decode<S>;

    fn layer(&self, inner: S) -> Self::Service {
        // NOTE: no specification governs this: our own design; the edge refused every
        // other coding, so the decoder passes nothing it was not told to read.
        let capped: fn(Request<DecompressionBody<Body>>) -> Request<Body> = cap;
        let decoder = RequestDecompression::new(MapRequest::new(inner, capped))
            .gzip(true)
            .br(true)
            .no_deflate()
            .no_zstd()
            .pass_through_unaccepted(true);
        let into_axum: fn(Response<UnsyncBoxBody<Bytes, BoxError>>) -> Response = into_axum;
        MapResponse::new(decoder, into_axum)
    }
}

/// The decoder's response, in an axum body.
fn into_axum(response: Response<UnsyncBoxBody<Bytes, BoxError>>) -> Response {
    response.map(Body::new)
}

/// The decoded request in an axum body, capped when the edge marked it
/// with a [`Watch`].
fn cap(request: Request<DecompressionBody<Body>>) -> Request<Body> {
    let watch = request.extensions().get::<Watch>().cloned();
    request.map(|body| match watch {
        Some(watch) => Body::new(Watched::decoded(Limited::new(body, watch.limit), watch)),
        None => Body::new(body),
    })
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, header};

    use super::admit;

    fn encoded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                header::CONTENT_ENCODING,
                HeaderValue::from_str(value).expect("a header value"),
            );
        }
        headers
    }

    #[test]
    fn gzip_x_gzip_and_br_are_admitted_in_lower_case() {
        for (written, read) in [
            ("gzip", "gzip"),
            ("GZip", "gzip"),
            ("x-gzip", "gzip"),
            (" br ", "br"),
            ("BR", "br"),
        ] {
            let mut headers = encoded(&[written]);
            assert!(admit(&mut headers).expect("admitted"), "{written}");
            assert_eq!(
                Some(read),
                headers
                    .get(header::CONTENT_ENCODING)
                    .and_then(|v| v.to_str().ok())
            );
        }
    }

    #[test]
    fn no_coding_or_an_empty_field_is_a_plain_body() {
        let mut headers = HeaderMap::new();
        assert!(!admit(&mut headers).expect("admitted"));
        let mut headers = encoded(&[""]);
        assert!(!admit(&mut headers).expect("admitted"));
        assert!(headers.get(header::CONTENT_ENCODING).is_none());
    }

    #[test]
    fn another_coding_or_more_than_one_is_refused() {
        for values in [
            &["zstd"][..],
            &["deflate"],
            &["identity"],
            &["gzip, br"],
            &["gzip", "gzip"],
        ] {
            let mut headers = encoded(values);
            assert!(admit(&mut headers).is_err(), "{values:?}");
        }
    }
}
