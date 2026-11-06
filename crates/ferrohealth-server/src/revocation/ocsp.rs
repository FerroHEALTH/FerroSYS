// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The status an OCSP responder gives one certificate (RFC 6960): the
//! request for it, and the check of the basic response, signed by the
//! certificate's issuer or by a responder the issuer authorised.
//!
//! The `CertID` hashes the issuer's name and key with SHA-256 (RFC 6960
//! §4.1.1 lets the requester choose), which leaves SHA-1 out: our own
//! design. The request carries no nonce, so a responder may answer from its
//! cache; the response's validity period bounds it instead (RFC 6960
//! §3.2, items 5 and 6).

use aws_lc_rs::digest::{self, SHA256};
use der::asn1::{ObjectIdentifier, OctetString};
use der::oid::AssociatedOid;
use der::{Decode, Encode};
use x509_cert::Certificate;
use x509_cert::ext::pkix::ExtendedKeyUsage;
use x509_cert::spki::AlgorithmIdentifierOwned;
use x509_ocsp::{
    BasicOcspResponse, CertId, CertStatus, OcspRequest, OcspResponse, OcspResponseStatus, Request,
    ResponderId, TbsRequest, Version,
};

use super::chain::{Link, same_name};
use super::crl::SKEW;
use super::{SignatureError, Standing, StatusError, signature};

/// `id-sha256` (RFC 5754 §2).
const SHA256_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");

/// `id-pkix-ocsp-basic` (RFC 6960 §4.2.1).
const OCSP_BASIC: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.48.1.1");

/// `id-kp-OCSPSigning` (RFC 6960 §4.2.2.2).
const OCSP_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");

/// How long, in seconds, a status with no `nextUpdate` is taken as current.
const FRESH_FOR: u64 = 300;

/// One OCSP request: the certificate it asks about and its DER.
#[derive(Debug)]
pub(super) struct Asked {
    id: CertId,
    body: Vec<u8>,
}

impl Asked {
    /// The request about `link`'s certificate.
    pub(super) fn new(link: &Link<'_>) -> Result<Self, StatusError> {
        let issuer = link.issuer.tbs_certificate();
        let name = issuer.subject().to_der().map_err(StatusError::Decode)?;
        let key = issuer
            .subject_public_key_info()
            .subject_public_key
            .as_bytes()
            .ok_or(StatusError::WrongIssuer)?;
        let hash = |bytes: &[u8]| OctetString::new(digest::digest(&SHA256, bytes).as_ref());
        let id = CertId {
            hash_algorithm: AlgorithmIdentifierOwned {
                oid: SHA256_OID,
                parameters: None,
            },
            issuer_name_hash: hash(&name).map_err(StatusError::Decode)?,
            issuer_key_hash: hash(key).map_err(StatusError::Decode)?,
            serial_number: link.subject.tbs_certificate().serial_number().clone(),
        };
        let request = OcspRequest {
            tbs_request: TbsRequest {
                version: Version::V1,
                requestor_name: None,
                request_list: vec![Request {
                    req_cert: id.clone(),
                    single_request_extensions: None,
                }],
                request_extensions: None,
            },
            optional_signature: None,
        };
        let body = request.to_der().map_err(StatusError::Decode)?;
        Ok(Self { id, body })
    }

    /// The DER of the request.
    pub(super) fn body(&self) -> Vec<u8> {
        self.body.clone()
    }

    /// What the DER OCSP response `answer` says of `link`'s certificate at
    /// the Unix second `now`.
    pub(super) fn standing(
        &self,
        answer: &[u8],
        link: &Link<'_>,
        now: u64,
    ) -> Result<Standing, StatusError> {
        let response = OcspResponse::from_der(answer).map_err(StatusError::Decode)?;
        if response.response_status != OcspResponseStatus::Successful {
            return Err(StatusError::Refused);
        }
        let bytes = response.response_bytes.ok_or(StatusError::Refused)?;
        if bytes.response_type != OCSP_BASIC {
            return Err(StatusError::Unsupported);
        }
        let basic =
            BasicOcspResponse::from_der(bytes.response.as_bytes()).map_err(StatusError::Decode)?;
        let signer = signer(&basic, link.issuer, now)?;
        let tbs = basic
            .tbs_response_data
            .to_der()
            .map_err(StatusError::Decode)?;
        let signature = basic
            .signature
            .as_bytes()
            .ok_or(StatusError::Signature(SignatureError::Invalid))?;
        signature::verify(
            signer.tbs_certificate().subject_public_key_info(),
            &basic.signature_algorithm,
            &tbs,
            signature,
        )
        .map_err(StatusError::Signature)?;
        let single = basic
            .tbs_response_data
            .responses
            .iter()
            .find(|single| same_id(&single.cert_id, &self.id))
            .ok_or(StatusError::NotAnswered)?;
        let this = single.this_update.0.to_unix_duration().as_secs();
        if this > now.saturating_add(SKEW) {
            return Err(StatusError::Stale);
        }
        // NOTE: RFC 6960 §4.2.2.1: with no nextUpdate newer information is always
        // available, so such a status is current for FRESH_FOR seconds (our own design).
        let until = match &single.next_update {
            Some(next) => next.0.to_unix_duration().as_secs(),
            None => this.saturating_add(FRESH_FOR),
        };
        if now >= until {
            return Err(StatusError::Stale);
        }
        match single.cert_status {
            CertStatus::Good(_) => Ok(Standing::Good { until }),
            CertStatus::Revoked(_) => Ok(Standing::Revoked),
            CertStatus::Unknown(_) => Err(StatusError::UnknownStatus),
        }
    }
}

/// Whether `a` and `b` identify the same certificate.
fn same_id(a: &CertId, b: &CertId) -> bool {
    a.hash_algorithm.oid == b.hash_algorithm.oid
        && a.issuer_name_hash == b.issuer_name_hash
        && a.issuer_key_hash == b.issuer_key_hash
        && a.serial_number == b.serial_number
}

/// The certificate that signed `basic`: `issuer` itself, or a responder
/// certificate in the response that `issuer` signed for OCSP signing and
/// that is valid at `now` (RFC 6960 §4.2.2.2).
fn signer<'a>(
    basic: &'a BasicOcspResponse,
    issuer: &'a Certificate,
    now: u64,
) -> Result<&'a Certificate, StatusError> {
    let responder = &basic.tbs_response_data.responder_id;
    if names(responder, issuer) {
        return Ok(issuer);
    }
    basic
        .certs
        .iter()
        .flatten()
        .find(|candidate| names(responder, candidate) && delegated(candidate, issuer, now))
        .ok_or(StatusError::NoResponder)
}

/// Whether `responder` names `certificate`, by its subject or by the SHA-1
/// of its key (RFC 6960 §4.2.1, `KeyHash`).
fn names(responder: &ResponderId, certificate: &Certificate) -> bool {
    let tbs = certificate.tbs_certificate();
    match responder {
        ResponderId::ByName(name) => same_name(name, tbs.subject()),
        ResponderId::ByKey(hash) => tbs
            .subject_public_key_info()
            .subject_public_key
            .as_bytes()
            .is_some_and(|key| {
                digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, key).as_ref() == hash.as_bytes()
            }),
    }
}

/// Whether `candidate` is a responder `issuer` authorised: issued and
/// signed by it, valid at `now`, with `id-kp-OCSPSigning`.
fn delegated(candidate: &Certificate, issuer: &Certificate, now: u64) -> bool {
    let tbs = candidate.tbs_certificate();
    let validity = tbs.validity();
    let valid = validity.not_before.to_unix_duration().as_secs() <= now.saturating_add(SKEW)
        && now < validity.not_after.to_unix_duration().as_secs();
    // NOTE: RFC 6960 §4.2.2.2: a responder certificate that does not verify is no
    // authorised responder, so the answer falls to NoResponder.
    let signed = same_name(tbs.issuer(), issuer.tbs_certificate().subject())
        && signature::verify_certificate(candidate, issuer).is_ok();
    let signing = tbs.extensions().into_iter().flatten().any(|extension| {
        extension.extn_id == ExtendedKeyUsage::OID
            && ExtendedKeyUsage::from_der(extension.extn_value.as_bytes())
                .is_ok_and(|usage| usage.0.contains(&OCSP_SIGNING))
    });
    valid && signed && signing
}
