// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The browser security headers of an operator console.
//!
//! Every response carries a Content-Security-Policy whose script source is a
//! nonce minted for that response alone (W3C Content Security Policy Level
//! 3), with `'wasm-unsafe-eval'` so the WebAssembly bundle compiles and
//! nothing admitting `'unsafe-inline'` or `'unsafe-eval'`. Beside it:
//! `nosniff`, `DENY` framing, no referrer, and a same-origin opener. A
//! document is marked `Cache-Control: no-store` and the site bundle
//! `no-cache` (RFC 9111 §5.2.2.4, §5.2.2.5), so a rebuilt bundle is fetched
//! again; a response that set its own `Cache-Control` keeps it.

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use http::{HeaderName, HeaderValue, StatusCode};

/// The `Cross-Origin-Opener-Policy` header.
const CROSS_ORIGIN_OPENER_POLICY: HeaderName =
    HeaderName::from_static("cross-origin-opener-policy");

/// The script nonce of one response, which the page renderer reads from the
/// request's extensions and writes into every script tag it emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CspNonce(String);

impl CspNonce {
    /// Mints a nonce: 128 random bits, hex-encoded.
    #[must_use]
    pub fn mint() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }

    /// Returns the nonce.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The policy's sources a console names: where its forms may post.
#[derive(Debug, Clone)]
pub struct ConsoleHeaders {
    form_action: Arc<str>,
    bundle: Option<Arc<str>>,
}

impl ConsoleHeaders {
    /// The headers of a console whose forms post to `form_action`, such as
    /// `'self'`.
    #[must_use]
    pub fn new(form_action: impl Into<Arc<str>>) -> Self {
        Self {
            form_action: form_action.into(),
            bundle: None,
        }
    }

    /// Serves the paths under `bundle`, such as `/pkg`, as the site bundle:
    /// `no-cache` on a successful or not-modified answer there, and `no-store`
    /// on every other response, a refusal under the bundle included.
    #[must_use]
    pub fn with_bundle(mut self, bundle: impl Into<Arc<str>>) -> Self {
        self.bundle = Some(bundle.into());
        self
    }

    /// Whether `path` is the bundle directory or under it.
    fn is_bundle(&self, path: &str) -> bool {
        let Some(bundle) = self.bundle.as_deref() else {
            return false;
        };
        let bundle = bundle.trim_end_matches('/');
        path.strip_prefix(bundle)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// The Content-Security-Policy that names `nonce`.
    #[must_use]
    pub fn policy(&self, nonce: &CspNonce) -> String {
        format!(
            "default-src 'self'; \
             script-src 'self' 'wasm-unsafe-eval' 'nonce-{nonce}'; \
             style-src 'self'; \
             img-src 'self' data:; \
             connect-src 'self'; \
             object-src 'none'; \
             base-uri 'self'; \
             form-action {form_action}; \
             frame-ancestors 'none'",
            nonce = nonce.as_str(),
            form_action = self.form_action
        )
    }
}

/// The middleware: mints the response's nonce, hands it to the renderer,
/// and sets the policy and the headers beside it.
///
/// Mount it with
/// `axum::middleware::from_fn(move |r, n| console::headers(headers.clone(), r, n))`.
pub async fn headers(headers: ConsoleHeaders, mut request: Request, next: Next) -> Response {
    let nonce = CspNonce::mint();
    let policy = HeaderValue::from_str(&headers.policy(&nonce));
    let bundle = headers.is_bundle(request.uri().path());
    request.extensions_mut().insert(nonce);
    let mut response = next.run(request).await;
    let status = response.status();
    let caching = if bundle && (status.is_success() || status == StatusCode::NOT_MODIFIED) {
        HeaderValue::from_static("no-cache")
    } else {
        HeaderValue::from_static("no-store")
    };
    let set = response.headers_mut();
    set.entry(CACHE_CONTROL).or_insert(caching);
    match policy {
        Ok(policy) => {
            set.insert(CONTENT_SECURITY_POLICY, policy);
        }
        Err(_invalid) => {
            // NOTE: no specification governs this: our own design; a policy that cannot
            // be a header falls back to the strictest one, which still blocks scripts.
            set.insert(
                CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'"),
            );
            tracing::error!("the Content-Security-Policy could not be written as a header");
        }
    }
    set.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    set.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    set.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    set.insert(
        CROSS_ORIGIN_OPENER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::routing::get;
    use tower::ServiceExt as _;

    use super::{ConsoleHeaders, CspNonce, headers};

    #[test]
    fn the_policy_allows_scripts_only_by_nonce_and_never_eval() {
        let nonce = CspNonce::mint();
        let policy = ConsoleHeaders::new("'self'").policy(&nonce);
        assert!(
            policy.contains(&format!(
                "script-src 'self' 'wasm-unsafe-eval' 'nonce-{}'",
                nonce.as_str()
            )),
            "{policy}"
        );
        assert!(
            !policy.contains("'unsafe-eval'") && !policy.contains("'unsafe-inline'"),
            "{policy}"
        );
        assert!(
            policy.contains("frame-ancestors 'none'") && policy.contains("form-action 'self'"),
            "{policy}"
        );
        assert_eq!(32, nonce.as_str().len());
        assert_ne!(nonce, CspNonce::mint(), "every response mints its own");
    }

    #[tokio::test]
    async fn every_response_carries_the_headers_and_the_renderer_sees_the_nonce() {
        let console = ConsoleHeaders::new("'self'");
        let app =
            Router::new()
                .route(
                    "/",
                    get(|Extension(nonce): Extension<CspNonce>| async move {
                        nonce.as_str().to_owned()
                    }),
                )
                .layer(axum::middleware::from_fn(move |request, next| {
                    headers(console.clone(), request, next)
                }));
        let response = app
            .oneshot(
                http::Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("the request is served");
        let get = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!("nosniff", get("x-content-type-options"));
        assert_eq!("DENY", get("x-frame-options"));
        assert_eq!("no-referrer", get("referrer-policy"));
        assert_eq!("same-origin", get("cross-origin-opener-policy"));
        let policy = get("content-security-policy");
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("the body");
        let nonce = String::from_utf8(body.to_vec()).expect("UTF-8");
        assert!(
            policy.contains(&format!("'nonce-{nonce}'")),
            "the header names the renderer's nonce: {policy}"
        );
    }

    async fn cache_control(app: &Router, uri: &str) -> String {
        let response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("the request builds"),
            )
            .await
            .expect("the request is served");
        response
            .headers()
            .get(http::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    #[tokio::test]
    async fn a_document_is_no_store_and_the_bundle_no_cache() {
        let console = ConsoleHeaders::new("'self'").with_bundle("/pkg");
        let app = Router::new()
            .route("/", get(|| async { "page" }))
            .route("/pkg/console.wasm", get(|| async { "wasm" }))
            .route("/pkgx", get(|| async { "page" }))
            .route(
                "/own",
                get(|| async { ([(http::header::CACHE_CONTROL, "max-age=60")], "own") }),
            )
            .layer(axum::middleware::from_fn(move |request, next| {
                headers(console.clone(), request, next)
            }));
        assert_eq!("no-store", cache_control(&app, "/").await);
        assert_eq!("no-cache", cache_control(&app, "/pkg/console.wasm").await);
        assert_eq!(
            "no-store",
            cache_control(&app, "/pkg/missing.js").await,
            "a refusal under the bundle is nothing to keep"
        );
        assert_eq!(
            "no-store",
            cache_control(&app, "/pkgx").await,
            "the bundle is a directory"
        );
        assert_eq!(
            "max-age=60",
            cache_control(&app, "/own").await,
            "a handler's own header stays"
        );
    }
}
