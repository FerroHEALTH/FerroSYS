// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! An ML-DSA-65+Ed25519 Transferable Secret Key, and the signer it unlocks
//! into.

use std::io::Read;

use pgp::composed::{Deserializable as _, DetachedSignature, SignedKeyDetails, SignedSecretKey};
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{
    KeyFlags, SecretKey, SignatureConfig, SignatureType, Subpacket, SubpacketData, UserId,
};
use pgp::types::{
    Fingerprint, KeyDetails as _, KeyVersion, PacketHeaderVersion, Password, PlainSecretParams,
    SecretParams, SignedUser, Tag, Timestamp,
};
use rand::rngs::OsRng;

use crate::certificate::Certificate;
use crate::composite::{self, CompositeSecretKey, HASH};
use crate::encoding::{self, Encoding};
use crate::error::Error;
use crate::mode::Mode;

/// A version 6 Transferable Secret Key (RFC 9580 §10.2) whose primary key
/// is ML-DSA-65+Ed25519, with the certificate it carries checked as
/// [`Certificate`] documents.
#[derive(Debug, Clone)]
pub struct Key {
    signed: SignedSecretKey,
    certificate: Certificate,
}

impl Key {
    /// Generates a key: a v6 ML-DSA-65+Ed25519 primary key with a
    /// direct-key self-signature that allows certifying and signing
    /// (RFC 9580 §10.1.1), and a positive certification of each User ID,
    /// the first marked primary. The key has no subkeys and is unlocked.
    ///
    /// # Errors
    ///
    /// [`Error::Provider`] when aws-lc-rs cannot draw randomness or expand
    /// the seeds, and [`Error::Packet`] when rPGP refuses a packet.
    pub fn generate(user_ids: &[&str]) -> Result<Self, Error> {
        let created = Timestamp::now();
        let (secret, material) = CompositeSecretKey::generate(created)?;
        let public = secret.public();
        let fingerprint = public.fingerprint();

        let mut flags = KeyFlags::default();
        flags.set_certify(true);
        flags.set_sign(true);
        let mut direct = SignatureConfig::v6(
            OsRng,
            SignatureType::Key,
            PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
            HASH,
        )?;
        direct.hashed_subpackets = vec![
            Subpacket::regular(SubpacketData::SignatureCreationTime(created))?,
            Subpacket::regular(SubpacketData::IssuerFingerprint(fingerprint.clone()))?,
            Subpacket::regular(SubpacketData::KeyFlags(flags))?,
            Subpacket::regular(SubpacketData::PreferredHashAlgorithms(
                vec![
                    HASH,
                    pgp::crypto::hash::HashAlgorithm::Sha3_512,
                    pgp::crypto::hash::HashAlgorithm::Sha256,
                    pgp::crypto::hash::HashAlgorithm::Sha3_256,
                ]
                .into(),
            ))?,
        ];
        let direct = direct.sign_key(&secret, &Password::empty(), public)?;

        let mut users = Vec::with_capacity(user_ids.len());
        for (index, user_id) in user_ids.iter().enumerate() {
            let id = UserId::from_str(PacketHeaderVersion::New, user_id)?;
            let mut certification = SignatureConfig::v6(
                OsRng,
                SignatureType::CertPositive,
                PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
                HASH,
            )?;
            certification.hashed_subpackets = vec![
                Subpacket::regular(SubpacketData::SignatureCreationTime(created))?,
                Subpacket::regular(SubpacketData::IssuerFingerprint(fingerprint.clone()))?,
                Subpacket::regular(SubpacketData::IsPrimary(index == 0))?,
            ];
            let signature = certification.sign_certification(
                &secret,
                public,
                &Password::empty(),
                Tag::UserId,
                &id,
            )?;
            users.push(SignedUser::new(id, vec![signature]));
        }

        let params = PlainSecretParams::try_from_reader_no_checksum(
            &material[..],
            KeyVersion::V6,
            PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
            public.packet().public_params(),
        )?;
        let primary = SecretKey::new(public.packet().clone(), SecretParams::Plain(params))?;
        let details = SignedKeyDetails::new(Vec::new(), vec![direct], users, Vec::new());
        Self::from_signed(SignedSecretKey::new(
            primary,
            details,
            Vec::new(),
            Vec::new(),
        ))
    }

    /// Reads every key in `input`, ASCII-armored or binary.
    ///
    /// # Errors
    ///
    /// [`Error::Packet`] when the input is not OpenPGP secret keys,
    /// [`Error::Missing`] when it holds none, and the errors of
    /// [`Key::from_signed`] for the first key that fails its checks.
    pub fn read_all(input: &[u8]) -> Result<Vec<Self>, Error> {
        let (keys, _headers) = SignedSecretKey::from_reader_many(input)?;
        let keys = keys
            .map(|signed| Self::from_signed(signed?))
            .collect::<Result<Vec<_>, _>>()?;
        if keys.is_empty() {
            return Err(Error::Missing { expected: "key" });
        }
        Ok(keys)
    }

    /// Reads exactly one key from `input`.
    ///
    /// # Errors
    ///
    /// The errors of [`Key::read_all`], and [`Error::Ambiguous`] when the
    /// input holds more than one key.
    pub fn read(input: &[u8]) -> Result<Self, Error> {
        let mut keys = Self::read_all(input)?;
        if keys.len() > 1 {
            return Err(Error::Ambiguous { expected: "key" });
        }
        keys.pop().ok_or(Error::Missing { expected: "key" })
    }

    /// Checks the certificate `signed` carries.
    ///
    /// # Errors
    ///
    /// The errors of [`Certificate::from_signed`].
    pub fn from_signed(signed: SignedSecretKey) -> Result<Self, Error> {
        let certificate = Certificate::from_signed(signed.to_public_key())?;
        Ok(Self {
            signed,
            certificate,
        })
    }

    /// The key as rPGP holds it.
    #[must_use]
    pub fn signed(&self) -> &SignedSecretKey {
        &self.signed
    }

    /// The certificate of this key, which carries no secret material.
    #[must_use]
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// The fingerprint of the primary key.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        self.certificate.fingerprint()
    }

    /// Whether the secret key material is locked with a password.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.signed.primary_key.secret_params().is_encrypted()
    }

    /// Locks the secret key material with `password`, with the S2K
    /// RFC 9580 gives version 6 keys: AEAD with an Argon2 derived key
    /// (§3.7.2.1, §5.5.3).
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyLocked`] when the key is locked, and
    /// [`Error::Packet`] when rPGP cannot encrypt the material.
    pub fn lock(&mut self, password: &Password) -> Result<(), Error> {
        if self.is_locked() {
            return Err(Error::AlreadyLocked);
        }
        self.signed.primary_key.set_password(OsRng, password)?;
        Ok(())
    }

    /// Unlocks the secret key material with `password` into a signer. An
    /// unlocked key ignores `password`.
    ///
    /// # Errors
    ///
    /// [`Error::Locked`] when `password` does not unlock the key, and the
    /// errors of [`CompositeSecretKey::from_material`] for material that
    /// does not belong to the public key.
    pub fn unlock(&self, password: &Password) -> Result<Signer, Error> {
        let material = self
            .signed
            .primary_key
            .unlock(password, |_public, plain| {
                composite::secret_material(plain)
                    .map_err(|error| pgp::errors::Error::from(error.to_string()))
            })
            .map_err(Error::Locked)??;
        let secret =
            CompositeSecretKey::from_material(self.certificate.public_key().clone(), &material)?;
        Ok(Signer {
            secret,
            certificate: self.certificate.clone(),
        })
    }

    /// Writes the key, with its secret material as it is (locked or not).
    ///
    /// # Errors
    ///
    /// [`Error::Packet`] when a packet cannot be serialized.
    pub fn encode(&self, encoding: Encoding) -> Result<Vec<u8>, Error> {
        encoding::encode(std::slice::from_ref(&self.signed), encoding)
    }
}

/// An unlocked key that makes detached signatures.
#[derive(Debug)]
pub struct Signer {
    secret: CompositeSecretKey,
    certificate: Certificate,
}

impl Signer {
    /// The certificate of the signing key.
    #[must_use]
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// The unlocked secret key, for rPGP's own signing APIs.
    #[must_use]
    pub fn secret_key(&self) -> &CompositeSecretKey {
        &self.secret
    }

    /// Makes a v6 detached signature over `data` with SHA2-512, carrying
    /// the signature creation time and the issuer fingerprint in its hashed
    /// area (RFC 9580 §5.2.3.11, §5.2.3.35).
    ///
    /// # Errors
    ///
    /// [`Error::OutsideValidity`] when the key has expired, and
    /// [`Error::Packet`] when reading `data` or building the packet fails.
    pub fn sign(&self, data: impl Read, mode: Mode) -> Result<DetachedSignature, Error> {
        let created = Timestamp::now();
        if !self.certificate.valid_at(created) {
            return Err(Error::OutsideValidity);
        }
        let mut config = SignatureConfig::v6(
            OsRng,
            mode.signature_type(),
            PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
            HASH,
        )?;
        config.hashed_subpackets = vec![
            Subpacket::regular(SubpacketData::SignatureCreationTime(created))?,
            Subpacket::regular(SubpacketData::IssuerFingerprint(
                self.certificate.fingerprint(),
            ))?,
        ];
        let signature = config.sign(&self.secret, &Password::empty(), data)?;
        Ok(DetachedSignature::new(signature))
    }
}
