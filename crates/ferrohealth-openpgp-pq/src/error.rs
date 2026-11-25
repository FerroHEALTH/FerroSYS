// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The one error type of the crate.

use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{SignatureType, SignatureVersion};
use pgp::types::KeyVersion;

/// A refusal or failure of a key, certificate or signature operation.
///
/// No variant carries secret key material, a password or signed data.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The key is not an RFC 9980 ML-DSA-65+Ed25519 key (algorithm ID 30).
    #[error(
        "the key uses public-key algorithm {found:?}; only ML-DSA-65+Ed25519 (RFC 9980 algorithm ID 30) is accepted"
    )]
    Algorithm {
        /// The algorithm the key packet names.
        found: PublicKeyAlgorithm,
    },
    /// The key is not a version 6 key (RFC 9980 §3.5).
    #[error(
        "the key is a {found:?} key; ML-DSA-65+Ed25519 is used only with version 6 keys (RFC 9980 §3.5)"
    )]
    KeyVersion {
        /// The version of the key packet.
        found: KeyVersion,
    },
    /// The key material has the wrong length (RFC 9980 §5.3.2).
    #[error("the {part} key material is {found} octets; RFC 9980 §5.3.2 fixes it at {expected}")]
    KeyMaterial {
        /// Which material: `public` or `secret`.
        part: &'static str,
        /// The length found.
        found: usize,
        /// The length RFC 9980 fixes.
        expected: usize,
    },
    /// The secret seeds do not derive the public key of the packet they are in.
    #[error("the secret key material does not derive the public key it is stored with")]
    KeyMismatch,
    /// aws-lc-rs refused the key material or could not draw randomness.
    #[error("the cryptographic provider refused the {operation}")]
    Provider {
        /// The operation the provider refused.
        operation: &'static str,
        /// The provider's error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The hash digest is shorter than 256 bits (RFC 9980 §5.3.1).
    #[error(
        "{hash:?} has a digest shorter than 256 bits, which RFC 9980 §5.3.1 refuses for ML-DSA-65+Ed25519"
    )]
    DigestTooShort {
        /// The hash algorithm the signature names.
        hash: HashAlgorithm,
    },
    /// The signature is not a version 6 signature (RFC 9980 §5.3.1).
    #[error(
        "the signature is a {found:?} signature; ML-DSA-65+Ed25519 is used only with version 6 signatures (RFC 9980 §5.3.1)"
    )]
    SignatureVersion {
        /// The version of the signature packet.
        found: SignatureVersion,
    },
    /// The signature is not a signature over a document.
    #[error(
        "the signature has type {found:?}; a detached signature is of type binary (0x00) or text (0x01)"
    )]
    SignatureType {
        /// The signature type, when the packet could be read.
        found: Option<SignatureType>,
    },
    /// The signature carries no hashed creation time (RFC 9580 §5.2.3.11).
    #[error("the signature has no hashed signature creation time (RFC 9580 §5.2.3.11)")]
    NoCreationTime,
    /// The signature names an issuer other than the certificate's key.
    #[error("the signature names another issuer than this certificate's key")]
    Issuer,
    /// The composite signature is not the 3373 octets RFC 9980 fixes.
    #[error(
        "the signature is {found} octets; an ML-DSA-65+Ed25519 signature is 3373 (RFC 9980 §5.3.1)"
    )]
    SignatureLength {
        /// The length found.
        found: usize,
    },
    /// The Ed25519 half does not verify.
    #[error("the Ed25519 signature does not verify")]
    Ed25519Invalid,
    /// The ML-DSA-65 half does not verify.
    #[error("the ML-DSA-65 signature does not verify")]
    MlDsaInvalid,
    /// The signature does not verify against the key.
    #[error("the signature does not verify")]
    Invalid(#[source] pgp::errors::Error),
    /// The signature was made before the key existed, or after it expired.
    #[error("the signature was made outside the key's validity period")]
    OutsideValidity,
    /// The certificate has no valid direct-key self-signature (RFC 9580 §5.2.3.10).
    #[error(
        "the certificate has no valid direct-key self-signature, which RFC 9580 §5.2.3.10 requires before a version 6 key is used"
    )]
    NoDirectKeySignature,
    /// The certificate carries a valid key revocation signature.
    #[error("the key is revoked")]
    Revoked,
    /// The key's direct-key self-signature does not carry the signing flag.
    #[error(
        "the key's direct-key self-signature does not carry the signing key flag 0x02 (RFC 9580 §5.2.3.29)"
    )]
    CannotSign,
    /// The secret key is already locked with a password.
    #[error("the secret key is already locked with a password")]
    AlreadyLocked,
    /// The secret key is locked and the password does not unlock it.
    #[error("the secret key is locked and the password does not unlock it")]
    Locked(#[source] pgp::errors::Error),
    /// The input holds no key, certificate or signature of the kind asked for.
    #[error("the input holds no {expected}")]
    Missing {
        /// What was expected.
        expected: &'static str,
    },
    /// The input holds more than one item where exactly one was asked for.
    #[error("the input holds more than one {expected}")]
    Ambiguous {
        /// What was expected.
        expected: &'static str,
    },
    /// rPGP refused the packets.
    #[error("OpenPGP packet handling failed")]
    Packet(#[from] pgp::errors::Error),
}
