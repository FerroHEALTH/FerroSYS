// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The TLS of an outbound link that checks every certificate the peer
//! presents for revocation before any data is sent.
//!
//! [`RevocationCheck::clear`] opens a TLS connection to the endpoint and
//! sends nothing over it. It reads the chain the peer presents, verified
//! against the trust roots alone, and checks every certificate in it but
//! the root against the sources the certificate names: first the CRLs of
//! its CRL distribution points (RFC 5280 §4.2.1.13, processed as §6.3
//! describes), then the OCSP responders of its Authority Information Access
//! extension (RFC 5280 §4.2.2.1, RFC 6960). The first source that answers
//! decides. A revoked certificate fails the check. So does a certificate
//! that no source answers for: a source that cannot be reached, a CRL that
//! is stale or carries a critical extension the check does not process, or
//! an OCSP status of `unknown`. The client [`RevocationCheck::http_client`]
//! builds admits only a chain a check cleared in the last five minutes, so
//! a request sent after a failed check, or with no check at all, ends at
//! the handshake and sends nothing.
//!
//! No specification governs the order of the sources, the five-minute
//! window, the hour a good status is kept, or failing closed when no source
//! answers: our own design.

mod chain;
mod crl;
mod fetch;
mod gate;
mod ocsp;
mod signature;

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use der::Decode;
use ferrohealth_secret::secret::Secret;
use rustls::client::{VerifierBuilderError, WebPkiServerVerifier};
use rustls::pki_types::pem::{self, PemObject};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::{Host, Url};
use x509_cert::Certificate;

use self::chain::Link;
use self::gate::{Cleared, Gate};
use crate::section::TlsVersion;

/// How long a cleared chain admits a connection, in seconds.
const CLEARED_FOR: u64 = 300;

/// How long a good status is kept before it is asked again, at most, in
/// seconds.
const KEPT_FOR: u64 = 3_600;

/// The TLS of an outbound link: the trust roots, the client identity, and
/// the chains a check cleared.
///
/// `Debug` shows how many roots it trusts and never the key.
#[derive(Clone)]
pub struct RevocationCheck {
    inner: Arc<Inner>,
}

struct Inner {
    anchors: Vec<Certificate>,
    probe: Arc<ClientConfig>,
    gated: ClientConfig,
    fetch: reqwest::Client,
    cleared: Arc<Cleared>,
    kept: Mutex<BTreeMap<[u8; 32], u64>>,
}

impl fmt::Debug for RevocationCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RevocationCheck")
            .field("roots", &self.inner.anchors.len())
            .finish_non_exhaustive()
    }
}

/// TLS material a check cannot be built from.
///
/// No variant carries any part of a certificate or a key.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RevocationSetupError {
    /// The trust roots hold no certificate.
    #[error("the trust roots hold no certificate")]
    NoRoots,
    /// The trust roots do not read as PEM.
    #[error("the trust roots do not read as PEM")]
    RootsPem(#[source] pem::Error),
    /// A trust root does not decode as an X.509 certificate.
    #[error("a trust root is not an X.509 certificate")]
    RootDer(#[source] der::Error),
    /// A trust root is refused as an anchor.
    #[error("a trust root is refused as a trust anchor")]
    Root(#[source] rustls::Error),
    /// The client identity holds no certificate.
    #[error("the client identity holds no certificate")]
    NoClientCertificate,
    /// The client identity does not read as PEM.
    #[error("the client identity does not read as PEM")]
    IdentityPem(#[source] pem::Error),
    /// The certificate verifier could not be built.
    #[error("the certificate verifier could not be built")]
    Verifier(#[source] VerifierBuilderError),
    /// The TLS configuration was refused.
    #[error("the TLS configuration was refused")]
    Tls(#[source] rustls::Error),
    /// An HTTP client could not be built.
    #[error("the HTTP client could not be built")]
    Client(#[source] reqwest::Error),
}

/// Why a check did not clear an endpoint. A depth counts from the peer's
/// own certificate, 0, towards the root.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RevocationError {
    /// The endpoint is not an `https` URL with a host.
    #[error("the endpoint is not an https URL with a host")]
    Endpoint,
    /// The check did not finish within its budget.
    #[error("the revocation check did not finish in time")]
    Timeout,
    /// The connection to the endpoint failed.
    #[error("the connection to the endpoint failed")]
    Connect(#[source] io::Error),
    /// The TLS handshake failed, an untrusted certificate among the causes.
    #[error("the TLS handshake with the endpoint failed")]
    Handshake(#[source] io::Error),
    /// The peer presented no certificate.
    #[error("the endpoint presented no certificate")]
    NoChain,
    /// A presented certificate does not decode.
    #[error("a certificate the endpoint presented does not decode")]
    Certificate(#[source] der::Error),
    /// A certificate's issuer is neither presented nor a trust root.
    #[error("the certificate at depth {depth} has no issuer among the chain and the trust roots")]
    NoIssuer {
        /// The certificate's depth.
        depth: usize,
    },
    /// A certificate names no source to check it by.
    #[error(
        "the certificate at depth {depth} names no CRL distribution point and no OCSP responder"
    )]
    NoSource {
        /// The certificate's depth.
        depth: usize,
    },
    /// A certificate is revoked.
    #[error("the certificate at depth {depth} is revoked")]
    Revoked {
        /// The certificate's depth.
        depth: usize,
    },
    /// No source gave a certificate's status; the error is the last
    /// source's.
    #[error("no revocation source gave the status of the certificate at depth {depth}")]
    Unavailable {
        /// The certificate's depth.
        depth: usize,
        /// Why the last source gave none.
        #[source]
        source: StatusError,
    },
}

/// Why one source gave no status.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StatusError {
    /// The source could not be asked.
    #[error("the revocation source could not be asked")]
    Fetch(#[source] reqwest::Error),
    /// The source answered with a status other than `200 OK`.
    #[error("the revocation source answered {0}")]
    Answered(http::StatusCode),
    /// The answer exceeds the size a source may send.
    #[error("the revocation source's answer is too large")]
    TooLarge,
    /// The answer does not decode.
    #[error("the revocation source's answer does not decode")]
    Decode(#[source] der::Error),
    /// The answer is not issued by the certificate's issuer.
    #[error("the revocation status is not issued by the certificate's issuer")]
    WrongIssuer,
    /// The answer's signature does not verify.
    #[error("the revocation status's signature does not verify")]
    Signature(#[source] SignatureError),
    /// The answer is not yet valid or no longer valid.
    #[error("the revocation status is outside its validity period")]
    Stale,
    /// The CRL carries a critical extension the check does not process
    /// (RFC 5280 §5.2 and §5.3).
    #[error("the CRL carries a critical extension the check does not process")]
    Unprocessable,
    /// The OCSP responder refused the request (RFC 6960 §4.2.1).
    #[error("the OCSP responder answered without a status")]
    Refused,
    /// The OCSP answer is not a basic response.
    #[error("the OCSP answer is not an id-pkix-ocsp-basic response")]
    Unsupported,
    /// No responder the issuer authorised signed the OCSP answer (RFC 6960
    /// §4.2.2.2).
    #[error("no authorised responder signed the OCSP answer")]
    NoResponder,
    /// The OCSP answer holds no status of the certificate asked about.
    #[error("the OCSP answer holds no status of the certificate")]
    NotAnswered,
    /// The OCSP responder does not know the certificate.
    #[error("the OCSP responder does not know the certificate")]
    UnknownStatus,
}

/// Why a signature on a certificate, a CRL or an OCSP response does not
/// verify.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SignatureError {
    /// The key and algorithm pair is not one the check verifies.
    #[error("the signature algorithm or key type is not one the check verifies")]
    Unsupported,
    /// The signed data or the signature does not encode.
    #[error("the signed data does not encode")]
    Encoding(#[source] der::Error),
    /// The signature is not the key's over the data.
    #[error("the signature does not verify")]
    Invalid,
}

/// What a source says of one certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Not revoked, until the Unix second given.
    Good {
        /// When the status stops being valid.
        until: u64,
    },
    /// Revoked.
    Revoked,
}

/// One source a certificate names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusSource {
    /// A CRL distribution point.
    Crl(Url),
    /// An OCSP responder.
    Ocsp(Url),
}

impl RevocationCheck {
    /// Reads `roots`, the PEM trust roots the link trusts the peer by (the
    /// platform's left out), and `identity`, the client certificate chain
    /// and private key in one PEM that the link presents when one is set;
    /// the link speaks TLS from `min_version` on, with the listener's
    /// cipher suites.
    ///
    /// # Errors
    ///
    /// A [`RevocationSetupError`] naming the part that does not read; it
    /// never carries the key.
    pub fn from_pem(
        roots: &str,
        identity: Option<&Secret>,
        min_version: TlsVersion,
    ) -> Result<Self, RevocationSetupError> {
        let roots = CertificateDer::pem_slice_iter(roots.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(RevocationSetupError::RootsPem)?;
        if roots.is_empty() {
            return Err(RevocationSetupError::NoRoots);
        }
        let mut store = RootCertStore::empty();
        let mut anchors = Vec::with_capacity(roots.len());
        for root in roots {
            anchors.push(Certificate::from_der(&root).map_err(RevocationSetupError::RootDer)?);
            store.add(root).map_err(RevocationSetupError::Root)?;
        }
        let identity = identity.map(client_identity).transpose()?;
        let provider = crate::tls::provider();
        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider.clone())
            .build()
            .map_err(RevocationSetupError::Verifier)?;
        let versions = crate::tls::protocol_versions(min_version);
        let probe = ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(versions)
            .map_err(RevocationSetupError::Tls)?
            .with_webpki_verifier(Arc::clone(&webpki));
        let probe = match &identity {
            None => probe.with_no_client_auth(),
            Some((chain, key)) => probe
                .with_client_auth_cert(chain.clone(), key.clone_key())
                .map_err(RevocationSetupError::Tls)?,
        };
        let cleared = Arc::new(Cleared::default());
        let gated = ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .map_err(RevocationSetupError::Tls)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Gate::new(webpki, Arc::clone(&cleared))));
        let gated = match identity {
            None => gated.with_no_client_auth(),
            Some((chain, key)) => gated
                .with_client_auth_cert(chain, key)
                .map_err(RevocationSetupError::Tls)?,
        };
        // NOTE: no specification governs this: our own design; a CRL or OCSP source
        // is asked with no redirect, so it answers from where the certificate names.
        let fetch = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(RevocationSetupError::Client)?;
        Ok(Self {
            inner: Arc::new(Inner {
                anchors,
                probe: Arc::new(probe),
                gated,
                fetch,
                cleared,
                kept: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// The HTTP client of the link: it presents the client identity, trusts
    /// the roots alone, follows no redirect, and completes a handshake only
    /// over a chain [`RevocationCheck::clear`] cleared.
    ///
    /// # Errors
    ///
    /// [`RevocationSetupError::Client`] when the client cannot be built.
    pub fn http_client(&self) -> Result<reqwest::Client, RevocationSetupError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured(self.inner.gated.clone())
            .build()
            .map_err(RevocationSetupError::Client)
    }

    /// Checks the chain `endpoint` presents within `within`, and on success
    /// admits it to the [`RevocationCheck::http_client`] connections for
    /// the next five minutes at most.
    ///
    /// # Errors
    ///
    /// A [`RevocationError`] when the endpoint cannot be reached, its chain
    /// is not trusted, or a certificate in it is revoked or has no status.
    pub async fn clear(&self, endpoint: &Url, within: Duration) -> Result<(), RevocationError> {
        let started = Instant::now();
        let deadline = started.checked_add(within).unwrap_or(started);
        let presented = tokio::time::timeout(within, self.presented(endpoint))
            .await
            .map_err(|_elapsed| RevocationError::Timeout)??;
        let parsed = presented
            .iter()
            .map(|der| Certificate::from_der(der))
            .collect::<Result<Vec<_>, _>>()
            .map_err(RevocationError::Certificate)?;
        let mut until = now().saturating_add(CLEARED_FOR);
        for link in chain::links(&parsed, &self.inner.anchors)? {
            until = until.min(self.standing(&link, deadline).await?);
        }
        self.inner.cleared.admit(gate::digest(&presented), until);
        Ok(())
    }

    /// The chain `endpoint` presents in a handshake that sends no data.
    async fn presented(
        &self,
        endpoint: &Url,
    ) -> Result<Vec<CertificateDer<'static>>, RevocationError> {
        if endpoint.scheme() != "https" {
            return Err(RevocationError::Endpoint);
        }
        let port = endpoint
            .port_or_known_default()
            .ok_or(RevocationError::Endpoint)?;
        let (name, stream) = match endpoint.host().ok_or(RevocationError::Endpoint)? {
            Host::Domain(domain) => (
                ServerName::try_from(domain.to_owned())
                    .map_err(|_invalid| RevocationError::Endpoint)?,
                TcpStream::connect((domain, port)).await,
            ),
            Host::Ipv4(address) => (
                ServerName::IpAddress(IpAddr::V4(address).into()),
                TcpStream::connect((address, port)).await,
            ),
            Host::Ipv6(address) => (
                ServerName::IpAddress(IpAddr::V6(address).into()),
                TcpStream::connect((address, port)).await,
            ),
        };
        let stream = stream.map_err(RevocationError::Connect)?;
        let tls = TlsConnector::from(Arc::clone(&self.inner.probe))
            .connect(name, stream)
            .await
            .map_err(RevocationError::Handshake)?;
        let presented = tls
            .get_ref()
            .1
            .peer_certificates()
            .ok_or(RevocationError::NoChain)?
            .iter()
            .map(|certificate| certificate.clone().into_owned())
            .collect::<Vec<_>>();
        if presented.is_empty() {
            return Err(RevocationError::NoChain);
        }
        Ok(presented)
    }

    /// Until when `link`'s certificate is good, asking its sources in turn
    /// before `deadline`.
    async fn standing(&self, link: &Link<'_>, deadline: Instant) -> Result<u64, RevocationError> {
        let depth = link.depth;
        let key = link.key().map_err(RevocationError::Certificate)?;
        if let Some(until) = self.kept(&key) {
            return Ok(until);
        }
        let sources = chain::sources(link.subject).map_err(RevocationError::Certificate)?;
        let mut last = None;
        for source in sources {
            let within = deadline.saturating_duration_since(Instant::now());
            if within.is_zero() {
                return Err(RevocationError::Timeout);
            }
            match self.ask(&source, link, within).await {
                Ok(Standing::Good { until }) => {
                    let until = until.min(now().saturating_add(KEPT_FOR));
                    self.keep(key, until);
                    return Ok(until);
                }
                Ok(Standing::Revoked) => return Err(RevocationError::Revoked { depth }),
                Err(error) => last = Some(error),
            }
        }
        match last {
            Some(source) => Err(RevocationError::Unavailable { depth, source }),
            None => Err(RevocationError::NoSource { depth }),
        }
    }

    /// What `source` says of `link`'s certificate, asked within `within`.
    async fn ask(
        &self,
        source: &StatusSource,
        link: &Link<'_>,
        within: Duration,
    ) -> Result<Standing, StatusError> {
        match source {
            StatusSource::Crl(url) => {
                let list = fetch::get(&self.inner.fetch, url, within).await?;
                crl::standing(&list, link, now())
            }
            StatusSource::Ocsp(url) => {
                let asked = ocsp::Asked::new(link)?;
                let answer = fetch::post(&self.inner.fetch, url, asked.body(), within).await?;
                asked.standing(&answer, link, now())
            }
        }
    }

    /// The kept good status of the certificate `key` names, while it lasts.
    fn kept(&self, key: &[u8; 32]) -> Option<u64> {
        let kept = self
            .inner
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        kept.get(key).copied().filter(|until| *until > now())
    }

    /// Keeps the good status of the certificate `key` names until `until`.
    fn keep(&self, key: [u8; 32], until: u64) {
        let mut kept = self
            .inner
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = now();
        kept.retain(|_, kept_until| *kept_until > now);
        kept.insert(key, until);
    }
}

/// The certificate chain and private key in the PEM `identity`.
fn client_identity(
    identity: &Secret,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), RevocationSetupError> {
    let pem = identity.expose().as_bytes();
    let chain = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(RevocationSetupError::IdentityPem)?;
    if chain.is_empty() {
        return Err(RevocationSetupError::NoClientCertificate);
    }
    let key = PrivateKeyDer::from_pem_slice(pem).map_err(RevocationSetupError::IdentityPem)?;
    Ok((chain, key))
}

/// The current Unix second; a clock before 1970 reads as 1970, at which
/// every status is not yet valid.
fn now() -> u64 {
    jiff::Timestamp::now().as_second().max(0).unsigned_abs()
}
