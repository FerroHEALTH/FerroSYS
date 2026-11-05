// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Asking a revocation source: a CRL by `GET` (RFC 5280 §4.2.1.13) and an
//! OCSP status by `POST` of `application/ocsp-request` (RFC 6960 Appendix
//! A.1), each answer bounded in size. The bounds are our own design: no
//! specification governs them.

use std::time::Duration;

use http::StatusCode;
use http::header::{ACCEPT, CONTENT_TYPE};
use url::Url;

use super::StatusError;

/// The largest CRL read.
const CRL_LIMIT: usize = 16 * 1024 * 1024;

/// The largest OCSP response read.
const OCSP_LIMIT: usize = 64 * 1024;

/// The CRL at `url`, asked within `within`.
pub(super) async fn get(
    client: &reqwest::Client,
    url: &Url,
    within: Duration,
) -> Result<Vec<u8>, StatusError> {
    let request = client
        .get(url.clone())
        .header(ACCEPT, "application/pkix-crl");
    read(request, within, CRL_LIMIT).await
}

/// The OCSP response `url` gives to `body`, asked within `within`.
pub(super) async fn post(
    client: &reqwest::Client,
    url: &Url,
    body: Vec<u8>,
    within: Duration,
) -> Result<Vec<u8>, StatusError> {
    let request = client
        .post(url.clone())
        .header(CONTENT_TYPE, "application/ocsp-request")
        .header(ACCEPT, "application/ocsp-response")
        .body(body);
    read(request, within, OCSP_LIMIT).await
}

/// The body `request` is answered with, read within `within` and at most
/// `limit` bytes long.
async fn read(
    request: reqwest::RequestBuilder,
    within: Duration,
    limit: usize,
) -> Result<Vec<u8>, StatusError> {
    let mut response = request
        .timeout(within)
        .send()
        .await
        .map_err(StatusError::Fetch)?;
    if response.status() != StatusCode::OK {
        return Err(StatusError::Answered(response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(StatusError::Fetch)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(StatusError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
