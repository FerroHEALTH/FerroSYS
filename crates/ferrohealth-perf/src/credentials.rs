// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What a load phase presents beside a request as written.
//!
//! That is the TLS client certificate of every connection (RFC 8705 §3), a
//! fresh `DPoP` proof per request (RFC 9449 §4), and a fresh
//! `private_key_jwt` client assertion per request (RFC 7523 §2.2).
//!
//! The plan names the files: `[load] client_identity_file` (a PEM
//! certificate chain and its private key), `[load] trust_roots_file` (the
//! roots that verify an `https` target), both read by `ferrohealth-server`'s
//! `TlsClient` as every FerroHEALTH product reads its outbound TLS keys, and
//! per request `dpop_key_file`, read by `ferrohealth-smart`'s `DpopKey`: a
//! P-256 or P-384 key signing ES256 or ES384, or an ML-DSA-44, ML-DSA-65 or
//! ML-DSA-87 key in PKCS#8 signing with its parameter set (RFC 9964 §2),
//! and per request `client_assertion.key_file`, read by `ferrohealth-smart`'s
//! `AssertionKey`. A relative path is read from the working directory. Each file is read when
//! the plan is, so a file that cannot be read or parsed refuses the plan
//! before any phase runs.

use std::path::Path;
use std::sync::Arc;

use ferrohealth_server::revocation::{Clearing, RevocationError};
use ferrohealth_server::tls_client::platform_tls::PlatformTls;
use ferrohealth_server::tls_client::{TlsClient, TlsClientError};
use ferrohealth_smart::client::assertion::AssertionKey;
use ferrohealth_smart::client::dpop::{DpopKey, Role};
use ferrohealth_smart::jws::algorithm::Algorithm;
use reqwest::ClientBuilder;
use thiserror::Error;
use url::Url;

use crate::plan::{ASSERTION_TYP, ClientAssertion, Plan};

/// The `client_assertion_type` of a JWT client assertion (RFC 7523 §2.2).
pub const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// A credential file the plan names that cannot be used.
#[derive(Debug, Error)]
pub enum CredentialError {
    /// The file could not be read.
    #[error("cannot read the {what} {path}")]
    Read {
        /// What the file holds.
        what: &'static str,
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: std::io::Error,
    },
    /// The file does not hold what it should.
    #[error("the {what} {path} does not parse")]
    Parse {
        /// What the file should hold.
        what: &'static str,
        /// The file.
        path: String,
        /// The cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A proof could not be signed.
    #[error("a DPoP proof could not be signed")]
    Sign {
        /// What the signer reported.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A client assertion could not be signed.
    #[error("a client assertion could not be signed")]
    Assertion {
        /// What the signer reported.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The client identity or the trust roots cannot be used.
    #[error("the TLS client material of [load] cannot be used")]
    Tls(#[from] TlsClientError),
    /// A target whose chain reaches a root of `[load] trust_roots_file` was
    /// not cleared for revocation, so no connection to it would open.
    #[error("the target {host} was not cleared for revocation")]
    Revocation {
        /// The target's host.
        host: String,
        /// Why the check did not clear it.
        #[source]
        source: RevocationError,
    },
}

/// How long one revocation check of a target may take.
const CLEAR_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

/// What the load client presents: its TLS identity and trust roots, and the
/// `DPoP` key and the client assertion signer of each request of the plan,
/// in the plan's order.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    tls: Option<Arc<PlatformTls>>,
    dpop: Vec<Option<Arc<DpopKey>>>,
    assertions: Vec<Option<Arc<Asserter>>>,
}

/// The signer of one request's client assertions.
#[derive(Debug)]
struct Asserter {
    key: AssertionKey,
    client_id: String,
    audience: Option<String>,
    typ: String,
}

impl Credentials {
    /// Reads every credential file `plan` names.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Tls`] for a client identity or trust roots file
    /// that cannot be read or holds no certificate chain and key or no
    /// certificate, and [`CredentialError::Read`] or
    /// [`CredentialError::Parse`] for a `DPoP` key that cannot be read or
    /// holds no P-256, P-384 or ML-DSA private key, or a client assertion
    /// key that cannot be read, holds no supported private key or cannot
    /// sign the table's algorithm.
    pub fn load(plan: &Plan) -> Result<Self, CredentialError> {
        let tls = TlsClient::read("load", &plan.load.tls())?.platform_config()?;
        let dpop = plan
            .requests
            .iter()
            .map(|request| {
                request
                    .dpop_key_file
                    .as_deref()
                    .map(|path| dpop_key(path).map(Arc::new))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let assertions = plan
            .requests
            .iter()
            .map(|request| {
                request
                    .client_assertion
                    .as_ref()
                    .map(|table| asserter(table).map(Arc::new))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            tls: Some(Arc::new(tls)),
            dpop,
            assertions,
        })
    }

    /// `builder` over the platform configuration of the plan's client
    /// identity, trust roots and oldest TLS version.
    pub fn apply(&self, builder: ClientBuilder) -> ClientBuilder {
        match &self.tls {
            Some(tls) => tls.apply(builder),
            None => builder,
        }
    }

    /// Clears the `https` ones of `targets` for revocation and keeps them
    /// cleared, when the plan
    /// names `[load] trust_roots_file`, so a connection to a target whose
    /// chain reaches one of those roots opens; `None` when it names none.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Revocation`] for a target the first round did not
    /// clear.
    pub async fn keep_cleared(
        &self,
        targets: Vec<Url>,
    ) -> Result<Option<Clearing>, CredentialError> {
        let Some(check) = self.tls.as_ref().and_then(|tls| tls.check()) else {
            return Ok(None);
        };
        let targets = targets
            .into_iter()
            .filter(|target| target.scheme() == "https")
            .collect();
        let (clearing, first) = Clearing::start(check.clone(), targets, CLEAR_WITHIN).await;
        for (target, outcome) in first {
            outcome.map_err(|source| CredentialError::Revocation {
                host: target.host_str().unwrap_or_default().to_owned(),
                source,
            })?;
        }
        Ok(Some(clearing))
    }

    /// A fresh proof for request `index` of the plan, sent with `method` to
    /// `url` and presenting `token` when it carries one (RFC 9449 §4.2), or
    /// `None` when the request names no `DPoP` key.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Sign`] when `method` is no HTTP method or the
    /// proof cannot be signed.
    pub fn proof(
        &self,
        index: usize,
        method: &str,
        url: &Url,
        token: Option<&str>,
    ) -> Result<Option<String>, CredentialError> {
        let Some(key) = self.dpop.get(index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let sign =
            |source: Box<dyn std::error::Error + Send + Sync>| CredentialError::Sign { source };
        let method = http::Method::from_bytes(method.as_bytes()).map_err(|e| sign(Box::new(e)))?;
        key.proof(&method, url, Role::Resource, token)
            .map(Some)
            .map_err(|e| sign(Box::new(e)))
    }
}

impl Credentials {
    /// The form fields of a fresh client assertion for request `index` of
    /// the plan sent to `url` (RFC 7523 §2.2): `client_assertion_type` and
    /// `client_assertion`, joined by `&`; `None` when the request names no
    /// `client_assertion`. Each carries a new `jti` and `iat` now, and its
    /// `aud` is the table's audience or `url` without query or fragment.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Assertion`] when no `jti` could be drawn or the
    /// assertion cannot be signed.
    pub fn assertion(&self, index: usize, url: &Url) -> Result<Option<String>, CredentialError> {
        let Some(asserter) = self.assertions.get(index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let audience = asserter.audience.clone().unwrap_or_else(|| {
            let mut endpoint = url.clone();
            endpoint.set_query(None);
            endpoint.set_fragment(None);
            endpoint.to_string()
        });
        let jwt = asserter
            .key
            .assertion(&asserter.client_id, &audience, &asserter.typ)
            .map_err(|source| CredentialError::Assertion {
                source: Box::new(source),
            })?;
        let fields = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_assertion_type", JWT_BEARER)
            .append_pair("client_assertion", &jwt)
            .finish();
        Ok(Some(fields))
    }
}

/// The signer the `client_assertion` table `table` names.
fn asserter(table: &ClientAssertion) -> Result<Asserter, CredentialError> {
    let path = table.key_file.display().to_string();
    let algorithm = table
        .algorithm
        .as_deref()
        .map(|name| {
            name.parse::<Algorithm>()
                .map_err(|source| CredentialError::Parse {
                    what: "client assertion algorithm",
                    path: name.to_owned(),
                    source: Box::new(source),
                })
        })
        .transpose()?;
    let key = AssertionKey::from_pem(
        &read(&table.key_file, "client assertion key")?,
        algorithm,
        table.kid.clone(),
    )
    .map_err(|source| CredentialError::Parse {
        what: "client assertion key",
        path,
        source: Box::new(source),
    })?;
    Ok(Asserter {
        key,
        client_id: table.client_id.clone(),
        audience: table.audience.clone(),
        typ: table
            .typ
            .clone()
            .unwrap_or_else(|| String::from(ASSERTION_TYP)),
    })
}

fn read(path: &Path, what: &'static str) -> Result<Vec<u8>, CredentialError> {
    std::fs::read(path).map_err(|source| CredentialError::Read {
        what,
        path: path.display().to_string(),
        source,
    })
}

/// The `DPoP` key in the PEM file `path`.
fn dpop_key(path: &Path) -> Result<DpopKey, CredentialError> {
    DpopKey::from_pem(&read(path, "DPoP key")?).map_err(|source| CredentialError::Parse {
        what: "DPoP key",
        path: path.display().to_string(),
        source: Box::new(source),
    })
}
