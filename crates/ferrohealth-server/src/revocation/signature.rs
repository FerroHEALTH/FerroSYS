// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The signature check of a certificate, a CRL and an OCSP response.
//!
//! The issuer's key verifies RSA PKCS#1 v1.5 and ECDSA with SHA-256,
//! SHA-384 or SHA-512. SHA-1 and any other algorithm, RSASSA-PSS among
//! them, are refused, so a status signed with one counts as not given. No
//! specification governs the set: our own design.

use aws_lc_rs::signature::{self, UnparsedPublicKey, VerificationAlgorithm};
use der::Encode;
use der::asn1::ObjectIdentifier;
use x509_cert::Certificate;
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};

use super::SignatureError;

const RSA_ENCRYPTION: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const SHA384_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const SHA512_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");
const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
const ECDSA_WITH_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
const P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const P384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
const P521: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.35");

/// Verifies `subject`'s signature with `issuer`'s key.
pub(super) fn verify_certificate(
    subject: &Certificate,
    issuer: &Certificate,
) -> Result<(), SignatureError> {
    let signed = subject
        .tbs_certificate
        .to_der()
        .map_err(SignatureError::Encoding)?;
    verify(
        &issuer.tbs_certificate.subject_public_key_info,
        &subject.signature_algorithm,
        &signed,
        subject
            .signature
            .as_bytes()
            .ok_or(SignatureError::Invalid)?,
    )
}

/// Verifies `signature` over `message` by the key `key` under `algorithm`.
pub(super) fn verify(
    key: &SubjectPublicKeyInfoOwned,
    algorithm: &AlgorithmIdentifierOwned,
    message: &[u8],
    signature: &[u8],
) -> Result<(), SignatureError> {
    let verification = verification(key, algorithm)?;
    let public = key
        .subject_public_key
        .as_bytes()
        .ok_or(SignatureError::Unsupported)?;
    UnparsedPublicKey::new(verification, public)
        .verify(message, signature)
        .map_err(|_unspecified| SignatureError::Invalid)
}

/// The verification algorithm of `algorithm` over a key of type `key`.
fn verification(
    key: &SubjectPublicKeyInfoOwned,
    algorithm: &AlgorithmIdentifierOwned,
) -> Result<&'static dyn VerificationAlgorithm, SignatureError> {
    if key.algorithm.oid == RSA_ENCRYPTION {
        return match algorithm.oid {
            oid if oid == SHA256_WITH_RSA => Ok(&signature::RSA_PKCS1_2048_8192_SHA256),
            oid if oid == SHA384_WITH_RSA => Ok(&signature::RSA_PKCS1_2048_8192_SHA384),
            oid if oid == SHA512_WITH_RSA => Ok(&signature::RSA_PKCS1_2048_8192_SHA512),
            _ => Err(SignatureError::Unsupported),
        };
    }
    if key.algorithm.oid != EC_PUBLIC_KEY {
        return Err(SignatureError::Unsupported);
    }
    let curve = key
        .algorithm
        .parameters
        .as_ref()
        .and_then(|parameters| parameters.decode_as::<ObjectIdentifier>().ok())
        .ok_or(SignatureError::Unsupported)?;
    let hash = algorithm.oid;
    match (curve, hash) {
        (c, h) if c == P256 && h == ECDSA_WITH_SHA256 => Ok(&signature::ECDSA_P256_SHA256_ASN1),
        (c, h) if c == P256 && h == ECDSA_WITH_SHA384 => Ok(&signature::ECDSA_P256_SHA384_ASN1),
        (c, h) if c == P256 && h == ECDSA_WITH_SHA512 => Ok(&signature::ECDSA_P256_SHA512_ASN1),
        (c, h) if c == P384 && h == ECDSA_WITH_SHA256 => Ok(&signature::ECDSA_P384_SHA256_ASN1),
        (c, h) if c == P384 && h == ECDSA_WITH_SHA384 => Ok(&signature::ECDSA_P384_SHA384_ASN1),
        (c, h) if c == P384 && h == ECDSA_WITH_SHA512 => Ok(&signature::ECDSA_P384_SHA512_ASN1),
        (c, h) if c == P521 && h == ECDSA_WITH_SHA256 => Ok(&signature::ECDSA_P521_SHA256_ASN1),
        (c, h) if c == P521 && h == ECDSA_WITH_SHA384 => Ok(&signature::ECDSA_P521_SHA384_ASN1),
        (c, h) if c == P521 && h == ECDSA_WITH_SHA512 => Ok(&signature::ECDSA_P521_SHA512_ASN1),
        _ => Err(SignatureError::Unsupported),
    }
}
