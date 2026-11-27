// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The signature check of a certificate, a CRL and an OCSP response.
//!
//! The issuer's key verifies RSA PKCS#1 v1.5 and ECDSA with SHA-256,
//! SHA-384 or SHA-512, and ML-DSA-44, ML-DSA-65 and ML-DSA-87 (RFC 9881).
//! SHA-1 and any other algorithm, RSASSA-PSS among them, are refused, so a
//! status signed with one counts as not given. No specification governs the
//! set: our own design.

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
const ID_ML_DSA_44: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.17");
const ID_ML_DSA_65: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.18");
const ID_ML_DSA_87: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.3.19");

/// Verifies `subject`'s signature with `issuer`'s key.
pub(super) fn verify_certificate(
    subject: &Certificate,
    issuer: &Certificate,
) -> Result<(), SignatureError> {
    let signed = subject
        .tbs_certificate()
        .to_der()
        .map_err(SignatureError::Encoding)?;
    verify(
        issuer.tbs_certificate().subject_public_key_info(),
        subject.signature_algorithm(),
        &signed,
        subject
            .signature()
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
    if let Some(ml_dsa) = ml_dsa(key.algorithm.oid) {
        // NOTE: RFC 9881 §2 and §3: the key's and the signature's identifier are the
        // same id-ml-dsa-* OID, and the parameters of both MUST be absent.
        let conforming = algorithm.oid == key.algorithm.oid
            && algorithm.parameters.is_none()
            && key.algorithm.parameters.is_none();
        return if conforming {
            Ok(ml_dsa)
        } else {
            Err(SignatureError::Unsupported)
        };
    }
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

/// The ML-DSA verification of the key identifier `oid`, which RFC 9881 §2
/// gives as one OID per parameter set; the signature is pure ML-DSA with the
/// empty context string (§3).
fn ml_dsa(oid: ObjectIdentifier) -> Option<&'static dyn VerificationAlgorithm> {
    match oid {
        o if o == ID_ML_DSA_44 => Some(&signature::ML_DSA_44),
        o if o == ID_ML_DSA_65 => Some(&signature::ML_DSA_65),
        o if o == ID_ML_DSA_87 => Some(&signature::ML_DSA_87),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::encoding::AsDer as _;
    use aws_lc_rs::signature::{
        KeyPair as _, ML_DSA_44_SIGNING, ML_DSA_65_SIGNING, ML_DSA_87_SIGNING, PqdsaKeyPair,
        PqdsaSigningAlgorithm,
    };
    use der::Decode as _;
    use der::asn1::{Any, ObjectIdentifier};
    use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};

    use super::{ID_ML_DSA_44, ID_ML_DSA_65, ID_ML_DSA_87, SignatureError, verify};

    const SETS: [(&PqdsaSigningAlgorithm, ObjectIdentifier); 3] = [
        (&ML_DSA_44_SIGNING, ID_ML_DSA_44),
        (&ML_DSA_65_SIGNING, ID_ML_DSA_65),
        (&ML_DSA_87_SIGNING, ID_ML_DSA_87),
    ];

    /// A generated key of `set`, its `SubjectPublicKeyInfo`, and its signature over `message`.
    fn signed(
        set: &'static PqdsaSigningAlgorithm,
        message: &[u8],
    ) -> (SubjectPublicKeyInfoOwned, Vec<u8>) {
        let pair = PqdsaKeyPair::generate(set).expect("a key");
        let spki = pair.public_key().as_der().expect("the key encodes");
        let spki = SubjectPublicKeyInfoOwned::from_der(spki.as_ref()).expect("the key decodes");
        let mut signature = vec![0; set.signature_len()];
        let written = pair.sign(message, &mut signature).expect("it signs");
        signature.truncate(written);
        (spki, signature)
    }

    fn named(oid: ObjectIdentifier) -> AlgorithmIdentifierOwned {
        AlgorithmIdentifierOwned {
            oid,
            parameters: None,
        }
    }

    #[test]
    fn every_ml_dsa_parameter_set_verifies() {
        for (set, oid) in SETS {
            let (spki, signature) = signed(set, b"tbsCertList");
            assert_eq!(oid, spki.algorithm.oid, "the key names its parameter set");
            assert!(
                verify(&spki, &named(oid), b"tbsCertList", &signature).is_ok(),
                "{oid}"
            );
            assert!(
                matches!(
                    verify(&spki, &named(oid), b"another list", &signature),
                    Err(SignatureError::Invalid)
                ),
                "{oid}: another message"
            );
        }
    }

    #[test]
    fn an_ml_dsa_signature_under_another_identifier_is_refused() {
        let (spki, signature) = signed(&ML_DSA_65_SIGNING, b"tbsResponseData");
        assert!(matches!(
            verify(&spki, &named(ID_ML_DSA_44), b"tbsResponseData", &signature),
            Err(SignatureError::Unsupported)
        ));
    }

    #[test]
    fn an_ml_dsa_identifier_with_parameters_is_refused() {
        let (spki, signature) = signed(&ML_DSA_65_SIGNING, b"tbsResponseData");
        let with = AlgorithmIdentifierOwned {
            oid: ID_ML_DSA_65,
            parameters: Some(Any::null()),
        };
        assert!(matches!(
            verify(&spki, &with, b"tbsResponseData", &signature),
            Err(SignatureError::Unsupported)
        ));
    }
}
