// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The ML-DSA-65+Ed25519 key material and its two component operations
//! (RFC 9980 §5), behind rPGP's key traits.
//!
//! Both components run in aws-lc-rs: Ed25519 is PureEdDSA with an empty
//! context (RFC 9980 §5.1.1) and ML-DSA-65 is the default hedged, pure
//! ML-DSA.Sign of FIPS 204 with an empty context (RFC 9980 §5.1.2, §9.3).
//! No rPGP cryptographic operation runs on this material.

use std::io;

use aws_lc_rs::signature::{
    ED25519, Ed25519KeyPair, KeyPair as _, ML_DSA_65, ML_DSA_65_SIGNING, PqdsaKeyPair,
    UnparsedPublicKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{PubKeyInner, PublicKey};
use pgp::ser::Serialize;
use pgp::types::{
    Fingerprint, KeyDetails, KeyId, KeyVersion, Password, PlainSecretParams, PublicParams,
    SignatureBytes, SigningKey, Timestamp, VerifyingKey,
};
use zeroize::Zeroizing;

use crate::error::Error;

/// The public-key algorithm ID of ML-DSA-65+Ed25519 (RFC 9980 §2.1).
///
/// `PublicKeyAlgorithm::from(ALGORITHM_ID)` is the right rPGP value whether
/// or not another crate in the build turns on rPGP's `pqc` feature.
pub const ALGORITHM_ID: u8 = 30;
/// An Ed25519 public key, secret key or seed (RFC 9980 Table 6).
pub const ED25519_KEY_LEN: usize = 32;
/// An ML-DSA-65 public key (RFC 9980 Table 7).
pub const ML_DSA_65_PUBLIC_LEN: usize = 1952;
/// An ML-DSA-65 secret key in seed format (RFC 9980 Table 7, §5.3.2.2).
pub const ML_DSA_65_SEED_LEN: usize = 32;
/// The public key material: Ed25519, then ML-DSA-65 (RFC 9980 §5.3.2.1).
pub const PUBLIC_LEN: usize = ED25519_KEY_LEN + ML_DSA_65_PUBLIC_LEN;
/// The secret key material: the Ed25519 secret, then the ML-DSA-65 seed
/// (RFC 9980 §5.3.2.2).
pub const SECRET_LEN: usize = ED25519_KEY_LEN + ML_DSA_65_SEED_LEN;
/// An Ed25519 signature (RFC 9980 Table 6).
pub const ED25519_SIGNATURE_LEN: usize = 64;
/// An ML-DSA-65 signature (RFC 9980 Table 7).
pub const ML_DSA_65_SIGNATURE_LEN: usize = 3309;
/// The composite signature: Ed25519, then ML-DSA-65 (RFC 9980 §5.3.1).
pub const SIGNATURE_LEN: usize = ED25519_SIGNATURE_LEN + ML_DSA_65_SIGNATURE_LEN;
/// The hash this crate signs with: SHA2-512, which RFC 9580 §9.5 asks every
/// implementation to support and which meets the 256-bit floor of RFC 9980
/// §5.3.1.
pub const HASH: HashAlgorithm = HashAlgorithm::Sha512;

/// Refuses a hash whose digest is shorter than 256 bits (RFC 9980 §5.3.1).
///
/// # Errors
///
/// [`Error::DigestTooShort`] for SHA2-224, SHA-1, MD5, RIPEMD-160 and any
/// hash whose digest size rPGP does not know.
pub fn check_digest(hash: HashAlgorithm) -> Result<(), Error> {
    match hash.digest_size() {
        Some(size) if size >= 32 => Ok(()),
        _ => Err(Error::DigestTooShort { hash }),
    }
}

/// The public half of an ML-DSA-65+Ed25519 v6 key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositePublicKey {
    packet: PublicKey,
    material: Vec<u8>,
}

impl CompositePublicKey {
    /// Builds the v6 public key packet for `material`, created at `created`.
    ///
    /// The parameters go through rPGP's own parser, so they take the form
    /// that parsing the same packet would give.
    fn from_material(material: &[u8], created: Timestamp) -> Result<Self, Error> {
        if material.len() != PUBLIC_LEN {
            return Err(Error::KeyMaterial {
                part: "public",
                found: material.len(),
                expected: PUBLIC_LEN,
            });
        }
        let params = PublicParams::try_from_reader(
            PublicKeyAlgorithm::from(ALGORITHM_ID),
            Some(PUBLIC_LEN),
            material,
        )?;
        let inner = PubKeyInner::new(
            KeyVersion::V6,
            PublicKeyAlgorithm::from(ALGORITHM_ID),
            created,
            None,
            params,
        )?;
        Self::try_from(PublicKey::from_inner(inner)?)
    }

    /// The rPGP public key packet.
    #[must_use]
    pub fn packet(&self) -> &PublicKey {
        &self.packet
    }

    /// The 32-octet Ed25519 public key.
    #[must_use]
    pub fn ed25519(&self) -> &[u8] {
        self.material.split_at(ED25519_KEY_LEN).0
    }

    /// The 1952-octet ML-DSA-65 public key.
    #[must_use]
    pub fn ml_dsa_65(&self) -> &[u8] {
        self.material.split_at(ED25519_KEY_LEN).1
    }

    /// Verifies a composite signature over `digest`, the dataDigest of RFC
    /// 9580 §5.2.4 computed with `hash`.
    ///
    /// Both component signatures must verify (RFC 9980 §3.2, §5.2.3).
    ///
    /// # Errors
    ///
    /// [`Error::DigestTooShort`] before any verification for a hash under 256
    /// bits, [`Error::SignatureLength`] for a signature that is not 3373
    /// octets, [`Error::Ed25519Invalid`] or [`Error::MlDsaInvalid`] for the
    /// first half that does not verify.
    pub fn verify_digest(
        &self,
        hash: HashAlgorithm,
        digest: &[u8],
        signature: &[u8],
    ) -> Result<(), Error> {
        check_digest(hash)?;
        if signature.len() != SIGNATURE_LEN {
            return Err(Error::SignatureLength {
                found: signature.len(),
            });
        }
        let (ed25519, ml_dsa) = signature.split_at(ED25519_SIGNATURE_LEN);
        UnparsedPublicKey::new(&ED25519, self.ed25519())
            .verify(digest, ed25519)
            .map_err(|_unspecified| Error::Ed25519Invalid)?;
        UnparsedPublicKey::new(&ML_DSA_65, self.ml_dsa_65())
            .verify(digest, ml_dsa)
            .map_err(|_unspecified| Error::MlDsaInvalid)
    }
}

impl TryFrom<PublicKey> for CompositePublicKey {
    type Error = Error;

    /// Accepts a v6 ML-DSA-65+Ed25519 key packet with key material of the
    /// length RFC 9980 §5.3.2.1 fixes.
    fn try_from(packet: PublicKey) -> Result<Self, Error> {
        if packet.algorithm() != PublicKeyAlgorithm::from(ALGORITHM_ID) {
            return Err(Error::Algorithm {
                found: packet.algorithm(),
            });
        }
        if packet.version() != KeyVersion::V6 {
            return Err(Error::KeyVersion {
                found: packet.version(),
            });
        }
        let material = packet.public_params().to_bytes()?;
        if material.len() != PUBLIC_LEN {
            return Err(Error::KeyMaterial {
                part: "public",
                found: material.len(),
                expected: PUBLIC_LEN,
            });
        }
        Ok(Self { packet, material })
    }
}

impl KeyDetails for CompositePublicKey {
    fn version(&self) -> KeyVersion {
        self.packet.version()
    }

    fn legacy_key_id(&self) -> KeyId {
        self.packet.legacy_key_id()
    }

    fn fingerprint(&self) -> Fingerprint {
        self.packet.fingerprint()
    }

    fn algorithm(&self) -> PublicKeyAlgorithm {
        self.packet.algorithm()
    }

    fn created_at(&self) -> Timestamp {
        self.packet.created_at()
    }

    fn legacy_v3_expiration_days(&self) -> Option<u16> {
        None
    }

    fn public_params(&self) -> &PublicParams {
        self.packet.public_params()
    }
}

impl Serialize for CompositePublicKey {
    fn to_writer<W: io::Write>(&self, writer: &mut W) -> pgp::errors::Result<()> {
        self.packet.to_writer(writer)
    }

    fn write_len(&self) -> usize {
        self.packet.write_len()
    }
}

impl VerifyingKey for CompositePublicKey {
    fn verify(
        &self,
        hash: HashAlgorithm,
        digest: &[u8],
        signature: &SignatureBytes,
    ) -> pgp::errors::Result<()> {
        let SignatureBytes::Native(bytes) = signature else {
            return Err(trait_error(&Error::SignatureLength { found: 0 }));
        };
        self.verify_digest(hash, digest, bytes)
            .map_err(|error| trait_error(&error))
    }
}

/// The secret half of an ML-DSA-65+Ed25519 v6 key, expanded for signing.
pub struct CompositeSecretKey {
    public: CompositePublicKey,
    ed25519: Ed25519KeyPair,
    ml_dsa: PqdsaKeyPair,
}

impl std::fmt::Debug for CompositeSecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeSecretKey")
            .field("fingerprint", &self.public.fingerprint())
            .finish_non_exhaustive()
    }
}

impl CompositeSecretKey {
    /// Generates the two component keys independently from fresh randomness
    /// (RFC 9980 §5.2.1) and returns the key with its secret material in the
    /// RFC 9980 §5.3.2.2 encoding.
    pub(crate) fn generate(created: Timestamp) -> Result<(Self, Zeroizing<Vec<u8>>), Error> {
        let mut secret = Zeroizing::new(vec![0_u8; SECRET_LEN]);
        aws_lc_rs::rand::fill(&mut secret).map_err(|source| Error::Provider {
            operation: "random seed generation",
            source: Box::new(source),
        })?;
        let (ed25519, ml_dsa) = expand(&secret)?;
        let mut material = Vec::with_capacity(PUBLIC_LEN);
        material.extend_from_slice(ed25519.public_key().as_ref());
        material.extend_from_slice(ml_dsa.public_key().as_ref());
        let public = CompositePublicKey::from_material(&material, created)?;
        Ok((
            Self {
                public,
                ed25519,
                ml_dsa,
            },
            secret,
        ))
    }

    /// Expands `secret`, the RFC 9980 §5.3.2.2 encoding, for the key `public`.
    ///
    /// # Errors
    ///
    /// [`Error::KeyMaterial`] when `secret` is not 64 octets,
    /// [`Error::Provider`] when aws-lc-rs refuses a seed, and
    /// [`Error::KeyMismatch`] when the seeds derive another public key.
    pub fn from_material(public: CompositePublicKey, secret: &[u8]) -> Result<Self, Error> {
        let (ed25519, ml_dsa) = expand(secret)?;
        if ed25519.public_key().as_ref() != public.ed25519()
            || ml_dsa.public_key().as_ref() != public.ml_dsa_65()
        {
            return Err(Error::KeyMismatch);
        }
        Ok(Self {
            public,
            ed25519,
            ml_dsa,
        })
    }

    /// The public half.
    #[must_use]
    pub fn public(&self) -> &CompositePublicKey {
        &self.public
    }

    /// Signs `digest`, the dataDigest of RFC 9580 §5.2.4 computed with
    /// `hash`: the Ed25519 signature, then the hedged ML-DSA-65 signature,
    /// both over the same digest (RFC 9980 §5.2.2).
    ///
    /// # Errors
    ///
    /// [`Error::DigestTooShort`] before any signing for a hash under 256
    /// bits, and [`Error::Provider`] when aws-lc-rs cannot sign.
    pub fn sign_digest(&self, hash: HashAlgorithm, digest: &[u8]) -> Result<Vec<u8>, Error> {
        check_digest(hash)?;
        let mut signature = vec![0_u8; SIGNATURE_LEN];
        let (ed25519, ml_dsa) = signature.split_at_mut(ED25519_SIGNATURE_LEN);
        ed25519.copy_from_slice(self.ed25519.sign(digest).as_ref());
        let written = self
            .ml_dsa
            .sign(digest, ml_dsa)
            .map_err(|source| Error::Provider {
                operation: "ML-DSA-65 signature",
                source: Box::new(source),
            })?;
        if written != ML_DSA_65_SIGNATURE_LEN {
            return Err(Error::SignatureLength {
                found: ED25519_SIGNATURE_LEN.saturating_add(written),
            });
        }
        Ok(signature)
    }
}

impl KeyDetails for CompositeSecretKey {
    fn version(&self) -> KeyVersion {
        self.public.version()
    }

    fn legacy_key_id(&self) -> KeyId {
        self.public.legacy_key_id()
    }

    fn fingerprint(&self) -> Fingerprint {
        self.public.fingerprint()
    }

    fn algorithm(&self) -> PublicKeyAlgorithm {
        self.public.algorithm()
    }

    fn created_at(&self) -> Timestamp {
        self.public.created_at()
    }

    fn legacy_v3_expiration_days(&self) -> Option<u16> {
        None
    }

    fn public_params(&self) -> &PublicParams {
        self.public.public_params()
    }
}

impl SigningKey for CompositeSecretKey {
    fn sign(
        &self,
        _key_pw: &Password,
        hash: HashAlgorithm,
        digest: &[u8],
    ) -> pgp::errors::Result<SignatureBytes> {
        self.sign_digest(hash, digest)
            .map(|signature| SignatureBytes::Native(signature.into()))
            .map_err(|error| trait_error(&error))
    }

    fn hash_alg(&self) -> HashAlgorithm {
        HASH
    }
}

/// The secret material of `params` in the RFC 9980 §5.3.2.2 encoding.
pub(crate) fn secret_material(params: &PlainSecretParams) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut secret = Zeroizing::new(Vec::with_capacity(SECRET_LEN));
    params.to_writer(&mut *secret, KeyVersion::V6)?;
    Ok(secret)
}

/// Expands the two seeds of the RFC 9980 §5.3.2.2 encoding.
fn expand(secret: &[u8]) -> Result<(Ed25519KeyPair, PqdsaKeyPair), Error> {
    if secret.len() != SECRET_LEN {
        return Err(Error::KeyMaterial {
            part: "secret",
            found: secret.len(),
            expected: SECRET_LEN,
        });
    }
    let (ed25519, ml_dsa) = secret.split_at(ED25519_KEY_LEN);
    let ed25519 =
        Ed25519KeyPair::from_seed_unchecked(ed25519).map_err(|source| Error::Provider {
            operation: "Ed25519 secret key",
            source: Box::new(source),
        })?;
    // NOTE: RFC 9980 §5.3.2.2, the seed xi is expanded with ML-DSA.KeyGen_internal (FIPS 204).
    let ml_dsa =
        PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, ml_dsa).map_err(|source| Error::Provider {
            operation: "ML-DSA-65 seed",
            source: Box::new(source),
        })?;
    Ok((ed25519, ml_dsa))
}

/// rPGP's traits return rPGP's error type, which carries a message only.
fn trait_error(error: &Error) -> pgp::errors::Error {
    pgp::errors::Error::from(error.to_string())
}
