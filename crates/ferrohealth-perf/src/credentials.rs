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
//! per request `dpop_key_file` (a P-256 private key in PKCS#8 PEM). A
//! relative path is read from the working directory. Each file is read when
//! the plan is, so a file that cannot be read or parsed refuses the plan
//! before any phase runs.

use std::path::Path;
use std::sync::Arc;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ferrohealth_server::tls_client::{TlsClient, TlsClientError};
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
        /// The cause, when the parser gave one.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// A proof could not be signed.
    #[error("a DPoP proof could not be signed")]
    Sign,
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
    /// holds no P-256 PKCS#8 key.
    pub fn load(plan: &Plan) -> Result<Self, CredentialError> {
        let tls = TlsClient::read("load", &plan.load.tls())?;
        let dpop = plan
            .requests
            .iter()
            .map(|request| {
                request
                    .dpop_key_file
                    .as_deref()
                    .map(|path| DpopKey::read(path).map(Arc::new))
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

    /// The `DPoP` key of request `index` of the plan, if it has one.
    #[must_use]
    pub fn dpop(&self, index: usize) -> Option<&Arc<DpopKey>> {
        self.dpop.get(index).and_then(Option::as_ref)
    }
}

fn read(path: &Path, what: &'static str) -> Result<Vec<u8>, CredentialError> {
    std::fs::read(path).map_err(|source| CredentialError::Read {
        what,
        path: path.display().to_string(),
        source,
    })
}

/// A P-256 key that signs `DPoP` proofs, with its public JWK.
pub struct DpopKey {
    pair: EcdsaKeyPair,
    jwk: serde_json::Value,
}

impl std::fmt::Debug for DpopKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DpopKey")
            .field("jwk", &self.jwk)
            .finish_non_exhaustive()
    }
}

impl DpopKey {
    /// Reads a P-256 private key in PKCS#8 PEM.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Read`] or [`CredentialError::Parse`].
    pub fn read(path: &Path) -> Result<Self, CredentialError> {
        let pem = read(path, "DPoP key")?;
        let parse =
            |source: Option<Box<dyn std::error::Error + Send + Sync>>| CredentialError::Parse {
                what: "DPoP key",
                path: path.display().to_string(),
                source,
            };
        let text = String::from_utf8(pem).map_err(|e| parse(Some(Box::new(e))))?;
        let body: String = text
            .lines()
            .skip_while(|line| line.trim() != "-----BEGIN PRIVATE KEY-----")
            .skip(1)
            .take_while(|line| line.trim() != "-----END PRIVATE KEY-----")
            .collect();
        let der = STANDARD
            .decode(body.trim())
            .map_err(|e| parse(Some(Box::new(e))))?;
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der)
            .map_err(|e| parse(Some(Box::new(e))))?;
        Self::of(pair).ok_or_else(|| parse(None))
    }

    /// The key of `pair` with its JWK; `None` when its public point is not
    /// an uncompressed P-256 point.
    fn of(pair: EcdsaKeyPair) -> Option<Self> {
        // NOTE: SEC 1 v2 §2.3.3: an uncompressed P-256 point is 0x04 followed by x and y of 32 bytes each.
        let point = pair.public_key().as_ref();
        let (x, y) = (point.get(1..33)?, point.get(33..65)?);
        let jwk = serde_json::json!({
            "kty": "EC",
            "crv": "P-256",
            "x": URL_SAFE_NO_PAD.encode(x),
            "y": URL_SAFE_NO_PAD.encode(y),
        });
        Some(Self { pair, jwk })
    }

    /// A fresh proof for `method` on `url` (RFC 9449 §4.2): a new `jti`,
    /// `iat` now, `htu` without query or fragment, and `ath` over `token`
    /// when the request carries one.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Sign`] when the signature fails.
    pub fn proof(
        &self,
        method: &str,
        url: &Url,
        token: Option<&str>,
    ) -> Result<String, CredentialError> {
        let mut htu = url.clone();
        htu.set_query(None);
        htu.set_fragment(None);
        let header = serde_json::json!({ "typ": "dpop+jwt", "alg": "ES256", "jwk": self.jwk });
        let mut claims = serde_json::json!({
            "jti": uuid::Uuid::new_v4().to_string(),
            "htm": method,
            "htu": htu.as_str(),
            "iat": jiff::Timestamp::now().as_second(),
        });
        if let (Some(token), Some(object)) = (token, claims.as_object_mut()) {
            let hash = digest(&SHA256, token.as_bytes());
            object.insert(
                "ath".to_owned(),
                serde_json::Value::from(URL_SAFE_NO_PAD.encode(hash.as_ref())),
            );
        }
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self
            .pair
            .sign(&SystemRandom::new(), input.as_bytes())
            .map_err(|_unspecified| CredentialError::Sign)?;
        Ok(format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }
}
