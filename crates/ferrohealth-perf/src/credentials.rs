// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The bindings a load phase presents for a sender-constrained token: the
//! TLS client certificate of every connection (RFC 8705 §3) and a fresh
//! `DPoP` proof per request (RFC 9449 §4).
//!
//! The plan names the files: `[load] client_identity_file` (a PEM
//! certificate chain and its private key), `[load] trust_roots_file` (the
//! roots that verify an `https` target), and per request `dpop_key_file` (a
//! P-256 private key in PKCS#8 PEM). A relative path is read from the working
//! directory. Each file is read when the plan is, so a file that cannot be
//! read or parsed refuses the plan before any phase runs.

use std::path::Path;
use std::sync::Arc;

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use reqwest::{Certificate, ClientBuilder, Identity};
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
}

/// What the load client presents: its TLS identity and trust roots, and the
/// `DPoP` key of each request of the plan, in the plan's order.
#[derive(Debug, Default, Clone)]
pub struct Credentials {
    identity: Option<Identity>,
    roots: Vec<Certificate>,
    dpop: Vec<Option<Arc<DpopKey>>>,
}

impl Credentials {
    /// Reads every credential file `plan` names.
    ///
    /// # Errors
    ///
    /// [`CredentialError::Read`] for a file that cannot be read, and
    /// [`CredentialError::Parse`] for one that holds no certificate chain
    /// and key, no certificate, or no P-256 PKCS#8 key.
    pub fn load(plan: &Plan) -> Result<Self, CredentialError> {
        let identity = plan
            .load
            .client_identity_file
            .as_deref()
            .map(|path| {
                let pem = read(path, "client identity")?;
                Identity::from_pem(&pem).map_err(|source| CredentialError::Parse {
                    what: "client identity",
                    path: path.display().to_string(),
                    source: Some(Box::new(source)),
                })
            })
            .transpose()?;
        let roots = match plan.load.trust_roots_file.as_deref() {
            None => Vec::new(),
            Some(path) => {
                let pem = read(path, "trust roots")?;
                let roots = Certificate::from_pem_bundle(&pem).map_err(|source| {
                    CredentialError::Parse {
                        what: "trust roots",
                        path: path.display().to_string(),
                        source: Some(Box::new(source)),
                    }
                })?;
                if roots.is_empty() {
                    return Err(CredentialError::Parse {
                        what: "trust roots",
                        path: path.display().to_string(),
                        source: None,
                    });
                }
                roots
            }
        };
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
            identity,
            roots,
            dpop,
        })
    }

    /// `builder` with the client identity and the trust roots.
    pub fn apply(&self, mut builder: ClientBuilder) -> ClientBuilder {
        if let Some(identity) = &self.identity {
            builder = builder.identity(identity.clone());
        }
        for root in &self.roots {
            builder = builder.add_root_certificate(root.clone());
        }
        builder
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
