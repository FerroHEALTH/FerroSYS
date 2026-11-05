// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The certificate verifier of the link's HTTP client: the path
//! verification against the trust roots, and then the chain must be one a
//! revocation check cleared, or the handshake fails with an unknown
//! revocation status and no data is sent. No specification governs the
//! gate: our own design.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use aws_lc_rs::digest::{Context, SHA256};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

/// The chains a check cleared, by digest, each with the Unix second it
/// stops admitting a connection.
#[derive(Debug, Default)]
pub(super) struct Cleared(Mutex<BTreeMap<[u8; 32], u64>>);

impl Cleared {
    /// Admits the chain `digest` names until `until`, and forgets every
    /// chain whose time has passed.
    pub(super) fn admit(&self, digest: [u8; 32], until: u64) {
        let mut cleared = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = super::now();
        cleared.retain(|_, admitted| *admitted > now);
        cleared.insert(digest, until);
    }

    /// Whether the chain `digest` names is admitted at the Unix second `now`.
    fn admits(&self, digest: &[u8; 32], now: u64) -> bool {
        let cleared = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cleared.get(digest).is_some_and(|until| *until > now)
    }
}

/// The SHA-256 over the presented chain `chain`, each certificate's length
/// and then its DER, in the order presented.
pub(super) fn digest(chain: &[CertificateDer<'_>]) -> [u8; 32] {
    let mut context = Context::new(&SHA256);
    for certificate in chain {
        context.update(
            &u64::try_from(certificate.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        context.update(certificate);
    }
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(context.finish().as_ref());
    digest
}

/// The verifier that admits a verified chain only when it is cleared.
#[derive(Debug)]
pub(super) struct Gate {
    webpki: Arc<WebPkiServerVerifier>,
    cleared: Arc<Cleared>,
}

impl Gate {
    /// The gate over `webpki`'s verification and the chains `cleared` holds.
    pub(super) fn new(webpki: Arc<WebPkiServerVerifier>, cleared: Arc<Cleared>) -> Self {
        Self { webpki, cleared }
    }
}

impl ServerCertVerifier for Gate {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let verified = self.webpki.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        let mut chain = Vec::with_capacity(intermediates.len().saturating_add(1));
        chain.push(end_entity.clone());
        chain.extend(intermediates.iter().cloned());
        if self.cleared.admits(&digest(&chain), now.as_secs()) {
            Ok(verified)
        } else {
            Err(Error::InvalidCertificate(
                CertificateError::UnknownRevocationStatus,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}
