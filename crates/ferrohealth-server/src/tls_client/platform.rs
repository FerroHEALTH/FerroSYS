// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The verifier of `TlsClient::platform_config`: a chain to a root the table
//! names is verified by webpki, a chain to any other root by the platform.
//!
//! A chain to a root of `trust_roots_file` passes webpki over the provider's
//! algorithms, ML-DSA among them (RFC 9881), and then the revocation gate:
//! only a chain a `RevocationCheck::clear` cleared opens a connection. A
//! chain to a root of `trust_roots_without_revocation_file` passes webpki
//! alone and is logged at each connection. Any other chain goes to the
//! platform's verifier under the operating system's policy. The platform
//! verifier is built once per process, since on Linux each build reads the
//! whole system root store (#372). No specification governs the split: our
//! own design.

use std::sync::{Arc, OnceLock};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};
use rustls_platform_verifier::Verifier;

/// The platform's verifier over `provider`, built on its first use in this
/// process and shared after.
///
/// # Errors
///
/// Returns the error of `Verifier::new` when the platform verifier cannot be
/// built.
fn platform(provider: Arc<CryptoProvider>) -> Result<Arc<Verifier>, Error> {
    static BUILT: OnceLock<Arc<Verifier>> = OnceLock::new();
    if let Some(verifier) = BUILT.get() {
        return Ok(Arc::clone(verifier));
    }
    let verifier = Arc::new(Verifier::new(provider)?);
    Ok(Arc::clone(BUILT.get_or_init(|| verifier)))
}

/// The roots trusted with no revocation check, with the names they are
/// logged by.
#[derive(Debug)]
pub(super) struct Unchecked {
    /// The webpki verifier over these roots alone.
    pub(super) webpki: Arc<WebPkiServerVerifier>,
    /// The table key, such as `xcpd`.
    pub(super) key: String,
    /// The roots' subjects, as the startup line and each connection name
    /// them.
    pub(super) names: String,
}

/// The verifier of a platform configuration.
#[derive(Debug)]
pub(super) struct Split {
    /// The revocation gate over the roots of `trust_roots_file`.
    checked: Option<Arc<dyn ServerCertVerifier>>,
    /// The roots of `trust_roots_without_revocation_file`.
    unchecked: Option<Unchecked>,
    platform: Arc<Verifier>,
    provider: Arc<CryptoProvider>,
}

impl Split {
    /// The verifier over the revocation gate `checked`, the roots
    /// `unchecked`, and the platform's verifier, on `provider`.
    ///
    /// # Errors
    ///
    /// Returns the error of the platform verifier when it cannot be built.
    pub(super) fn new(
        checked: Option<Arc<dyn ServerCertVerifier>>,
        unchecked: Option<Unchecked>,
        provider: Arc<CryptoProvider>,
    ) -> Result<Self, Error> {
        Ok(Self {
            checked,
            unchecked,
            platform: platform(Arc::clone(&provider))?,
            provider,
        })
    }
}

/// Whether `result` says the chain reaches none of the verifier's roots, so
/// the next verifier decides.
fn elsewhere(result: &Result<ServerCertVerified, Error>) -> bool {
    matches!(
        result,
        Err(Error::InvalidCertificate(CertificateError::UnknownIssuer))
    )
}

impl ServerCertVerifier for Split {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if let Some(checked) = &self.checked {
            let result = checked.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            );
            if !elsewhere(&result) {
                return result;
            }
        }
        if let Some(unchecked) = &self.unchecked {
            let result = unchecked.webpki.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            );
            if !elsewhere(&result) {
                if result.is_ok() {
                    tracing::warn!(
                        key = %unchecked.key,
                        roots = %unchecked.names,
                        server = %server_name.to_str(),
                        "a connection is trusted with no revocation check: its chain reaches a root of trust_roots_without_revocation_file"
                    );
                }
                return result;
            }
        }
        self.platform
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::platform;

    #[test]
    fn the_platform_verifier_is_built_once_and_shared() {
        let provider = crate::tls::provider();
        let first = platform(Arc::clone(&provider)).expect("a verifier");
        let again = platform(provider).expect("a verifier");
        assert!(Arc::ptr_eq(&first, &again), "the system store is read once");
    }
}
