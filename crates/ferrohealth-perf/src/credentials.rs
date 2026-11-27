// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The bindings a load phase presents for a sender-constrained token: the
//! TLS client certificate of every connection (RFC 8705 §3) and a fresh
//! `DPoP` proof per request (RFC 9449 §4).
//!
//! The plan names the files: `[load] client_identity_file` (a PEM
//! certificate chain and its private key), `[load] trust_roots_file` (the
//! roots that verify an `https` target), both read by `ferrohealth-server`'s
//! `TlsClient` as every FerroHEALTH product reads its outbound TLS keys, and
//! per request `dpop_key_file`, read by `ferrohealth-smart`'s `DpopKey`: a
//! P-256 or P-384 key signing ES256 or ES384, or an ML-DSA-44, ML-DSA-65 or
//! ML-DSA-87 key in PKCS#8 signing with its parameter set (RFC 9964 §2). A
//! relative path is read from the working directory. Each file is read when
//! the plan is, so a file that cannot be read or parsed refuses the plan
//! before any phase runs.

use std::path::Path;
use std::sync::Arc;

use ferrohealth_server::tls_client::{TlsClient, TlsClientError};
use ferrohealth_smart::client::dpop::{DpopKey, Role};
use reqwest::ClientBuilder;
use thiserror::Error;
use url::Url;

use crate::plan::Plan;

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
    /// The client identity or the trust roots cannot be used.
    #[error("the TLS client material of [load] cannot be used")]
    Tls(#[from] TlsClientError),
}

/// What the load client presents: its TLS identity and trust roots, and the
/// `DPoP` key of each request of the plan, in the plan's order.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    tls: Option<Arc<TlsClient>>,
    dpop: Vec<Option<Arc<DpopKey>>>,
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
    /// holds no P-256, P-384 or ML-DSA private key.
    pub fn load(plan: &Plan) -> Result<Self, CredentialError> {
        let tls = TlsClient::read("load", &plan.load.tls())?;
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
        Ok(Self {
            tls: Some(Arc::new(tls)),
            dpop,
        })
    }

    /// `builder` with the client identity, the trust roots and the oldest
    /// TLS version of the plan.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Tls`] when reqwest refuses the identity or a root.
    pub fn apply(&self, builder: ClientBuilder) -> Result<ClientBuilder, CredentialError> {
        match &self.tls {
            Some(tls) => Ok(tls.apply(builder)?),
            None => Ok(builder),
        }
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
