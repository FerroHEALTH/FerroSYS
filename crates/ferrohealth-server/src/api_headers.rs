// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The security headers of an API response, which the stack sets outermost.
//!
//! The set follows the OWASP Secure Headers Project
//! (<https://owasp.org/www-project-secure-headers/>) for a JSON API whose
//! responses carry patient data:
//!
//! | Header | Value | Authority |
//! | --- | --- | --- |
//! | `Cache-Control` | `no-store` | RFC 9111 §5.2.2.5 |
//! | `X-Content-Type-Options` | `nosniff` | WHATWG Fetch, the `X-Content-Type-Options` header |
//! | `Referrer-Policy` | `strict-origin-when-cross-origin` | W3C Referrer Policy §3 |
//! | `Cross-Origin-Resource-Policy` | `same-site` | WHATWG Fetch, the `Cross-Origin-Resource-Policy` header |
//! | `X-Frame-Options` | `DENY` | RFC 7034 §2.1 |
//! | `Content-Security-Policy` | `default-src 'none'; frame-ancestors 'none'` | W3C Content Security Policy Level 3 §6.1.3, §6.4.2 |
//! | `Strict-Transport-Security` | `max-age=31536000`, only when the listener serves TLS | RFC 6797 §6.1, §7.2 |
//!
//! `no-store` keeps a patient record out of every cache, and an `ETag` beside
//! it still serves a conditional request. The referrer policy keeps a path
//! that names a record out of a cross-origin `Referer`. The policy and the
//! framing refusal cover the HTML an API origin can still serve, such as an
//! error page.
//!
//! The stack sets a header only when the response lacks it, so a route that
//! sets its own `Content-Security-Policy` or `Cache-Control` keeps it, and an
//! operator console keeps every header the `console` feature's middleware
//! sets. A route tree that answers with headers of its own design
//! opts out with [`exempt`]; the refusals the stack writes itself never reach
//! that tree, so they keep the set.
//!
//! `Strict-Transport-Security` is sent only when the section's listener
//! serves TLS itself (`[server.tls]`). A browser ignores the header over
//! plain HTTP (RFC 6797 §8.1), and the server must not send it there
//! (§7.2); behind a proxy that terminates TLS, the proxy owns it.
//! `includeSubDomains` is left out, because a product does not decide for
//! the hosts beside it (§6.1.2).

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use http::{HeaderName, HeaderValue};

/// The `Cross-Origin-Resource-Policy` header.
const CROSS_ORIGIN_RESOURCE_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-resource-policy");

/// The headers every API response carries, with their values.
const SET: [(HeaderName, &str); 6] = [
    (CACHE_CONTROL, "no-store"),
    (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (REFERRER_POLICY, "strict-origin-when-cross-origin"),
    (CROSS_ORIGIN_RESOURCE_POLICY, "same-site"),
    (X_FRAME_OPTIONS, "DENY"),
    (
        CONTENT_SECURITY_POLICY,
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

/// One year, the `max-age` the OWASP Secure Headers Project names.
const HSTS: &str = "max-age=31536000";

/// The marker [`exempt`] puts on a response.
#[derive(Debug, Clone, Copy)]
struct Exempt;

/// Marks a response as one the stack adds no security header to.
///
/// Mount it on a route tree whose headers are its own, with
/// `axum::middleware::map_response(api_headers::exempt)`.
///
/// # Examples
///
/// ```
/// use axum::Router;
/// use axum::routing::get;
/// use ferrohealth_server::api_headers;
///
/// let site: Router = Router::new()
///     .route("/", get(|| async { "<!doctype html>" }))
///     .layer(axum::middleware::map_response(api_headers::exempt));
/// # let _ = site;
/// ```
#[expect(
    clippy::unused_async,
    reason = "axum's map_response middleware takes an async function"
)]
pub async fn exempt(mut response: Response) -> Response {
    response.extensions_mut().insert(Exempt);
    response
}

/// Whether the listener serves TLS itself, which admits
/// `Strict-Transport-Security`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Transport {
    pub(crate) tls: bool,
}

/// The middleware the stack mounts outermost.
pub(crate) async fn set(
    State(transport): State<Transport>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    if response.extensions().get::<Exempt>().is_some() {
        return response;
    }
    let headers = response.headers_mut();
    for (name, value) in SET {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    }
    if transport.tls {
        headers
            .entry(STRICT_TRANSPORT_SECURITY)
            .or_insert(HeaderValue::from_static(HSTS));
    }
    response
}
