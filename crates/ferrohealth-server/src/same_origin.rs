// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The same-origin check of an operator console's state-changing requests.
//!
//! A console signs its operator in with a session cookie, so another site
//! could post a form that acts with that session (OWASP Cross-Site Request
//! Forgery Prevention Cheat Sheet). [`only`] refuses every request whose
//! method is not safe (RFC 9110 §9.2.1) unless it shows the console's own
//! origin: `Sec-Fetch-Site` decides when the browser sends it, and only
//! `same-origin` passes (W3C Fetch Metadata Request Headers §2.4); without
//! it `Origin` must be the console's origin (RFC 6454 §7), and without that
//! the origin of `Referer` must be (RFC 9110 §10.1.3). A request that shows
//! none of them is refused, so an unsafe request is never taken on trust.
//!
//! A console answers with `Referrer-Policy: no-referrer`, under which a
//! browser sends a plain form post with `Origin: null` and no `Referer`
//! (WHATWG Fetch, the `Origin` header), so the console's own forms pass by
//! `Sec-Fetch-Site`, and a browser without fetch metadata fails closed. The
//! `Origin` and `Referer` fallbacks serve a call the page makes with
//! `fetch`.

use std::fmt;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use ferrohealth_telemetry::request_log::RequestId;
use http::header::{CACHE_CONTROL, ORIGIN, REFERER};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use url::Url;

use crate::refusal::{RefusalBody, default_body};

/// The fetch metadata header that names where a request comes from.
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Why a request that is not a safe method was refused.
///
/// The refusal [`only`] writes carries it in the response's extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossOrigin {
    /// `Sec-Fetch-Site` named something other than `same-origin`.
    FetchSite,
    /// `Origin` named another origin, `null` among them.
    Origin,
    /// The origin of `Referer` was another one, or `Referer` was no URL.
    Referer,
    /// The request showed no origin, or the console knows none of its own.
    Unknown,
}

impl CrossOrigin {
    /// The reason in plain words, the text of the refusal.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::FetchSite => "the browser says this request came from another site",
            Self::Origin => "the request names another origin than the console's",
            Self::Referer => "the request was sent from a page of another origin",
            Self::Unknown => "the request does not show that it came from the console's own pages",
        }
    }
}

impl fmt::Display for CrossOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for CrossOrigin {}

/// The public URL a console was given has no origin of its own, such as a
/// `file:` or `data:` URL (RFC 6454 §4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the console's public URL `{scheme}:` has no tuple origin")]
pub struct OpaqueOrigin {
    scheme: String,
}

/// The console's own origin and the body its refusals are written in.
#[derive(Clone)]
pub struct SameOrigin {
    origin: Option<Arc<str>>,
    body: RefusalBody,
}

impl fmt::Debug for SameOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SameOrigin")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl Default for SameOrigin {
    fn default() -> Self {
        Self {
            origin: None,
            body: default_body(),
        }
    }
}

impl SameOrigin {
    /// A check that knows no origin of the console: only `Sec-Fetch-Site:
    /// same-origin` passes an unsafe request, and the refusal is plain text.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The console is served at `public_url`, whose origin `Origin` and
    /// `Referer` are compared with.
    ///
    /// # Errors
    ///
    /// [`OpaqueOrigin`] when `public_url` has no tuple origin.
    pub fn with_origin(mut self, public_url: &Url) -> Result<Self, OpaqueOrigin> {
        let origin = public_url.origin();
        if !origin.is_tuple() {
            return Err(OpaqueOrigin {
                scheme: public_url.scheme().to_owned(),
            });
        }
        self.origin = Some(origin.ascii_serialization().into());
        Ok(self)
    }

    /// Writes the refusal with `body`, the product's own error shape, as the
    /// stack's refusals are ([`crate::refusal`]).
    #[must_use]
    pub fn with_body(mut self, body: RefusalBody) -> Self {
        self.body = body;
        self
    }

    /// Checks `headers` of a request that is not a safe method.
    ///
    /// # Errors
    ///
    /// The [`CrossOrigin`] reason when the headers do not show the console's
    /// own origin.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), CrossOrigin> {
        if let Some(site) = headers.get(SEC_FETCH_SITE) {
            return if site.as_bytes() == b"same-origin" {
                Ok(())
            } else {
                Err(CrossOrigin::FetchSite)
            };
        }
        let Some(origin) = self.origin.as_deref() else {
            return Err(CrossOrigin::Unknown);
        };
        if let Some(sent) = headers.get(ORIGIN) {
            return if sent.as_bytes() == origin.as_bytes() {
                Ok(())
            } else {
                Err(CrossOrigin::Origin)
            };
        }
        let Some(referer) = headers.get(REFERER) else {
            return Err(CrossOrigin::Unknown);
        };
        // NOTE: RFC 9110 §10.1.3: a `Referer` that is no URL names no origin, so it
        // is refused as one of another origin is.
        let same = referer
            .to_str()
            .ok()
            .and_then(|referer| Url::parse(referer).ok())
            .is_some_and(|referer| referer.origin().ascii_serialization() == origin);
        if same {
            Ok(())
        } else {
            Err(CrossOrigin::Referer)
        }
    }
}

/// The middleware: refuses an unsafe request that [`SameOrigin::check`] does not pass.
///
/// A safe method always passes. The refusal is `403` in the product's body,
/// marked `Cache-Control: no-store`, written before the handler or anything
/// that reads the session runs.
///
/// Mount it with
/// `axum::middleware::from_fn(move |r, n| same_origin::only(check.clone(), r, n))`.
pub async fn only(check: SameOrigin, request: Request, next: Next) -> Response {
    if request.method().is_safe() {
        return next.run(request).await;
    }
    let Err(refused) = check.check(request.headers()) else {
        return next.run(request).await;
    };
    tracing::warn!(
        method = %request.method(),
        reason = refused.reason(),
        "a request that did not come from the console's own pages was refused"
    );
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.id().to_owned())
        .unwrap_or_default();
    let mut response = (check.body)(
        request.uri(),
        &request_id,
        StatusCode::FORBIDDEN,
        refused.reason(),
    );
    response
        .headers_mut()
        .entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response.extensions_mut().insert(refused);
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::response::IntoResponse as _;
    use axum::routing::post;
    use http::{HeaderMap, Method, StatusCode};
    use tower::ServiceExt as _;
    use url::Url;

    use super::{CrossOrigin, OpaqueOrigin, SameOrigin, only};

    const CONSOLE: &str = "https://console.example.org:8443";

    fn check() -> SameOrigin {
        SameOrigin::new()
            .with_origin(&Url::parse("https://console.example.org:8443/ui/").expect("a URL"))
            .expect("a tuple origin")
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("a header value"));
        }
        map
    }

    #[test]
    fn sec_fetch_site_decides_alone_when_sent() {
        let check = check();
        assert_eq!(
            Ok(()),
            check.check(&headers(&[("sec-fetch-site", "same-origin")]))
        );
        for site in ["same-site", "cross-site", "none"] {
            let sent = headers(&[("sec-fetch-site", site)]);
            assert_eq!(Err(CrossOrigin::FetchSite), check.check(&sent), "{site}");
        }
        let forged = headers(&[("sec-fetch-site", "cross-site"), ("origin", CONSOLE)]);
        assert_eq!(
            Err(CrossOrigin::FetchSite),
            check.check(&forged),
            "a matching Origin does not overrule the fetch metadata"
        );
    }

    #[test]
    fn a_plain_form_with_origin_null_passes_by_sec_fetch_site() {
        let sent = headers(&[("sec-fetch-site", "same-origin"), ("origin", "null")]);
        assert_eq!(Ok(()), check().check(&sent));
        assert_eq!(
            Ok(()),
            SameOrigin::new().check(&sent),
            "the fetch metadata needs no configured origin"
        );
    }

    #[test]
    fn origin_decides_without_fetch_metadata() {
        let check = check();
        assert_eq!(Ok(()), check.check(&headers(&[("origin", CONSOLE)])));
        for origin in [
            "null",
            "https://console.example.org",
            "https://evil.example.org:8443",
        ] {
            let sent = headers(&[
                ("origin", origin),
                ("referer", "https://console.example.org:8443/"),
            ]);
            assert_eq!(Err(CrossOrigin::Origin), check.check(&sent), "{origin}");
        }
    }

    #[test]
    fn referer_decides_without_origin() {
        let check = check();
        let own = headers(&[("referer", "https://console.example.org:8443/mappings?x=1")]);
        assert_eq!(Ok(()), check.check(&own));
        let other = headers(&[("referer", "https://evil.example.org/form")]);
        assert_eq!(Err(CrossOrigin::Referer), check.check(&other));
        let no_url = headers(&[("referer", "console.example.org:8443/mappings")]);
        assert_eq!(Err(CrossOrigin::Referer), check.check(&no_url), "no URL");
    }

    #[test]
    fn no_header_or_no_origin_is_refused() {
        assert_eq!(Err(CrossOrigin::Unknown), check().check(&HeaderMap::new()));
        assert_eq!(
            Err(CrossOrigin::Unknown),
            SameOrigin::new().check(&headers(&[("origin", CONSOLE)])),
            "a console that knows no origin takes no Origin on trust"
        );
    }

    #[test]
    fn an_opaque_public_url_is_refused() {
        let opaque = Url::parse("data:text/plain,console").expect("a URL");
        assert!(matches!(
            SameOrigin::new().with_origin(&opaque),
            Err(OpaqueOrigin { .. })
        ));
    }

    fn app(check: SameOrigin) -> Router {
        Router::new()
            .route("/act", post(|| async { "done" }).get(|| async { "page" }))
            .layer(axum::middleware::from_fn(move |request, next| {
                only(check.clone(), request, next)
            }))
    }

    async fn send(
        app: &Router,
        method: Method,
        pairs: &[(&'static str, &'static str)],
    ) -> axum::response::Response {
        let mut request = http::Request::builder().method(method).uri("/act");
        for (name, value) in pairs {
            request = request.header(*name, *value);
        }
        app.clone()
            .oneshot(request.body(Body::empty()).expect("the request builds"))
            .await
            .expect("the request is served")
    }

    #[tokio::test]
    async fn a_safe_method_always_passes() {
        let app = app(check());
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            let response = send(&app, method.clone(), &[("sec-fetch-site", "cross-site")]).await;
            assert_ne!(StatusCode::FORBIDDEN, response.status(), "{method}");
        }
    }

    #[tokio::test]
    async fn an_unsafe_method_from_another_site_is_refused_in_the_products_body() {
        let body: crate::refusal::RefusalBody = Arc::new(|uri, _id, status, reason| {
            (
                status,
                format!("{{\"path\":\"{}\",\"reason\":\"{reason}\"}}", uri.path()),
            )
                .into_response()
        });
        let app = app(check().with_body(body));
        let passed = send(&app, Method::POST, &[("sec-fetch-site", "same-origin")]).await;
        assert_eq!(StatusCode::OK, passed.status());
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            let refused = send(&app, method.clone(), &[("sec-fetch-site", "cross-site")]).await;
            assert_eq!(StatusCode::FORBIDDEN, refused.status(), "{method}");
            assert_eq!(
                Some(&CrossOrigin::FetchSite),
                refused.extensions().get::<CrossOrigin>()
            );
            assert_eq!(
                Some("no-store"),
                refused
                    .headers()
                    .get(http::header::CACHE_CONTROL)
                    .and_then(|value| value.to_str().ok())
            );
            let text = axum::body::to_bytes(refused.into_body(), 1024)
                .await
                .expect("the body");
            assert_eq!(
                format!(
                    "{{\"path\":\"/act\",\"reason\":\"{}\"}}",
                    CrossOrigin::FetchSite.reason()
                ),
                String::from_utf8(text.to_vec()).expect("UTF-8")
            );
        }
        let missing = send(&app, Method::POST, &[]).await;
        assert_eq!(StatusCode::FORBIDDEN, missing.status());
    }
}
