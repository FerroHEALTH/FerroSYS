// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A validated ML-DSA-65+Ed25519 certificate and the verification of
//! detached signatures against it.

use std::io::Read;

use pgp::composed::{Deserializable as _, SignedPublicKey};
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{Signature, SignatureType, SignatureVersion};
use pgp::types::{Fingerprint, KeyDetails as _, Timestamp};

use crate::composite::{self, CompositePublicKey};
use crate::encoding::{self, Encoding};
use crate::error::Error;
use crate::mode::Mode;

/// A version 6 Transferable Public Key (RFC 9580 §10.1.1) whose primary key
/// is ML-DSA-65+Ed25519, checked on construction.
///
/// The checks: the primary key is a v6 algorithm 30 key, a valid
/// direct-key self-signature exists (RFC 9580 §5.2.3.10), no valid key
/// revocation exists, and the newest valid direct-key self-signature carries
/// the signing key flag (RFC 9580 §5.2.3.29). Its key expiration time, if
/// any, bounds the signatures the certificate accepts.
#[derive(Debug, Clone)]
pub struct Certificate {
    signed: SignedPublicKey,
    key: CompositePublicKey,
    expires: Option<Timestamp>,
}

/// A detached signature that verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verification {
    created: Timestamp,
    mode: Mode,
}

impl Verification {
    /// The signature creation time.
    #[must_use]
    pub fn created(self) -> Timestamp {
        self.created
    }

    /// Whether the signature covers bytes or canonical text.
    #[must_use]
    pub fn mode(self) -> Mode {
        self.mode
    }
}

impl Certificate {
    /// Reads every certificate in `input`, ASCII-armored or binary.
    ///
    /// # Errors
    ///
    /// [`Error::Packet`] when the input is not OpenPGP certificates,
    /// [`Error::Missing`] when it holds none, and the errors of
    /// [`Certificate::from_signed`] for the first certificate that fails its
    /// checks.
    pub fn read_all(input: &[u8]) -> Result<Vec<Self>, Error> {
        let (certificates, _headers) = SignedPublicKey::from_reader_many(input)?;
        let certificates = certificates
            .map(|signed| Self::from_signed(signed?))
            .collect::<Result<Vec<_>, _>>()?;
        if certificates.is_empty() {
            return Err(Error::Missing {
                expected: "certificate",
            });
        }
        Ok(certificates)
    }

    /// Reads exactly one certificate from `input`.
    ///
    /// # Errors
    ///
    /// The errors of [`Certificate::read_all`], and [`Error::Ambiguous`] when
    /// the input holds more than one certificate.
    pub fn read(input: &[u8]) -> Result<Self, Error> {
        let mut certificates = Self::read_all(input)?;
        if certificates.len() > 1 {
            return Err(Error::Ambiguous {
                expected: "certificate",
            });
        }
        certificates.pop().ok_or(Error::Missing {
            expected: "certificate",
        })
    }

    /// Checks `signed` as the type documents.
    ///
    /// # Errors
    ///
    /// [`Error::Algorithm`], [`Error::KeyVersion`] or [`Error::KeyMaterial`]
    /// for a primary key that is not a v6 ML-DSA-65+Ed25519 key,
    /// [`Error::Revoked`], [`Error::NoDirectKeySignature`] and
    /// [`Error::CannotSign`].
    pub fn from_signed(signed: SignedPublicKey) -> Result<Self, Error> {
        let key = CompositePublicKey::try_from(signed.primary_key.clone())?;
        let revoked = signed
            .details
            .revocation_signatures
            .iter()
            .any(|revocation| self_signature(revocation) && revocation.verify_key(&key).is_ok());
        if revoked {
            return Err(Error::Revoked);
        }
        let newest = signed
            .details
            .direct_signatures
            .iter()
            .filter(|direct| self_signature(direct) && direct.verify_key(&key).is_ok())
            .filter_map(|direct| Some((direct.created()?, direct)))
            .max_by_key(|(created, _)| *created)
            .map(|(_, direct)| direct)
            .ok_or(Error::NoDirectKeySignature)?;
        if !newest.key_flags().sign() {
            return Err(Error::CannotSign);
        }
        let expires = newest
            .key_expiration_time()
            .filter(|lifetime| lifetime.as_secs() > 0)
            .map(|lifetime| {
                Timestamp::from_secs(
                    key.created_at()
                        .as_secs()
                        .saturating_add(lifetime.as_secs()),
                )
            });
        Ok(Self {
            signed,
            key,
            expires,
        })
    }

    /// The fingerprint of the primary key (RFC 9580 §5.5.4.3).
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        self.key.fingerprint()
    }

    /// The primary key.
    #[must_use]
    pub fn public_key(&self) -> &CompositePublicKey {
        &self.key
    }

    /// The certificate as rPGP holds it.
    #[must_use]
    pub fn signed(&self) -> &SignedPublicKey {
        &self.signed
    }

    /// When the key expires, if its direct-key self-signature sets a key
    /// expiration time.
    #[must_use]
    pub fn expires(&self) -> Option<Timestamp> {
        self.expires
    }

    /// Whether the key is valid at `time`: not before its creation and not
    /// at or after its expiration.
    #[must_use]
    pub fn valid_at(&self, time: Timestamp) -> bool {
        time >= self.key.created_at() && self.expires.is_none_or(|expires| time < expires)
    }

    /// Verifies a detached signature by this certificate's primary key over
    /// `data`.
    ///
    /// The signature's form is checked before any hashing: a v6 signature
    /// of type binary or text, by algorithm 30, over a hash of 256 bits or
    /// more, with a hashed creation time inside the key's validity and, if
    /// it names an issuer fingerprint, this key's.
    ///
    /// # Errors
    ///
    /// [`Error::SignatureVersion`], [`Error::SignatureType`],
    /// [`Error::Algorithm`], [`Error::DigestTooShort`],
    /// [`Error::NoCreationTime`], [`Error::Issuer`] and
    /// [`Error::OutsideValidity`] for a signature of the wrong form, and
    /// [`Error::Invalid`] for one that does not verify.
    pub fn verify(&self, signature: &Signature, data: impl Read) -> Result<Verification, Error> {
        if signature.version() != SignatureVersion::V6 {
            return Err(Error::SignatureVersion {
                found: signature.version(),
            });
        }
        let mode = signature
            .typ()
            .and_then(Mode::of)
            .ok_or(Error::SignatureType {
                found: signature.typ(),
            })?;
        let config = signature
            .config()
            .ok_or(Error::SignatureType { found: None })?;
        if config.pub_alg != PublicKeyAlgorithm::from(composite::ALGORITHM_ID) {
            return Err(Error::Algorithm {
                found: config.pub_alg,
            });
        }
        composite::check_digest(config.hash_alg)?;
        let created = signature.created().ok_or(Error::NoCreationTime)?;
        let issuers = signature.issuer_fingerprint();
        if !issuers.is_empty() && !issuers.contains(&&self.fingerprint()) {
            return Err(Error::Issuer);
        }
        if !self.valid_at(created) {
            return Err(Error::OutsideValidity);
        }
        signature.verify(&self.key, data).map_err(Error::Invalid)?;
        Ok(Verification { created, mode })
    }

    /// Writes the certificate.
    ///
    /// # Errors
    ///
    /// [`Error::Packet`] when a packet cannot be serialized.
    pub fn encode(&self, encoding: Encoding) -> Result<Vec<u8>, Error> {
        encoding::encode(std::slice::from_ref(&self.signed), encoding)
    }
}

/// Writes `certificates` as one block, a keyring when there are several.
///
/// # Errors
///
/// [`Error::Packet`] when a packet cannot be serialized.
pub fn encode_all(certificates: &[Certificate], encoding: Encoding) -> Result<Vec<u8>, Error> {
    let signed: Vec<_> = certificates
        .iter()
        .map(|certificate| certificate.signed.clone())
        .collect();
    encoding::encode(&signed, encoding)
}

/// Whether a key signature is a v6 signature by algorithm 30 over a hash of
/// 256 bits or more, the only form a self-signature of this key can take.
fn self_signature(signature: &Signature) -> bool {
    signature.version() == SignatureVersion::V6
        && signature.config().is_some_and(|config| {
            config.pub_alg == PublicKeyAlgorithm::from(composite::ALGORITHM_ID)
                && composite::check_digest(config.hash_alg).is_ok()
                && matches!(
                    config.typ,
                    SignatureType::Key | SignatureType::KeyRevocation
                )
        })
}
