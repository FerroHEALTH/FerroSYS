// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Every refusal: the digest floor, tampered halves, keys and signatures of
//! the wrong form, and certificates that fail their checks.

use ferrohealth_openpgp_pq::certificate::Certificate;
use ferrohealth_openpgp_pq::composite::{self, CompositeSecretKey, HASH, SIGNATURE_LEN};
use ferrohealth_openpgp_pq::error::Error;
use ferrohealth_openpgp_pq::key::Key;
use pgp::composed::{KeyType, SecretKeyParamsBuilder, SignedPublicKey};
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{
    KeyFlags, PubKeyInner, PublicKey, Signature, SignatureConfig, SignatureType, Subpacket,
    SubpacketData,
};
use pgp::types::{
    Duration, KeyDetails as _, KeyVersion, Password, PublicParams, SignatureBytes, Timestamp,
};
use rand::rngs::OsRng;

use crate::support::{a3_certificate, a3_key, generated};

/// A signature packet of `config` whose composite bytes are all zero: its
/// form is what the case checks, so it is refused before any verification.
fn unsigned(config: SignatureConfig) -> Signature {
    Signature::from_config(
        config,
        [0, 0],
        SignatureBytes::Native(vec![0; SIGNATURE_LEN].into()),
    )
    .unwrap()
}

fn config(typ: SignatureType, hash: HashAlgorithm, subpackets: Vec<Subpacket>) -> SignatureConfig {
    let mut config = SignatureConfig::v6(
        OsRng,
        typ,
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        hash,
    )
    .unwrap();
    config.hashed_subpackets = subpackets;
    config
}

fn created_now() -> Subpacket {
    Subpacket::regular(SubpacketData::SignatureCreationTime(Timestamp::now())).unwrap()
}

/// A v6 key of `key_type` made by rPGP's own generator.
fn rpgp_key(key_type: KeyType, version: KeyVersion) -> pgp::composed::SignedSecretKey {
    let mut params = SecretKeyParamsBuilder::default();
    params
        .key_type(key_type)
        .version(version)
        .can_sign(true)
        .primary_user_id("Other <other@example.org>".into());
    params.build().unwrap().generate(OsRng).unwrap()
}

#[test]
fn the_digest_floor_holds_on_sign_and_on_verify() {
    for short in [
        HashAlgorithm::Sha224,
        HashAlgorithm::Sha1,
        HashAlgorithm::Md5,
        HashAlgorithm::Ripemd160,
    ] {
        assert!(
            matches!(
                composite::check_digest(short),
                Err(Error::DigestTooShort { .. })
            ),
            "{short:?} is under 256 bits"
        );
    }
    for long in [
        HashAlgorithm::Sha256,
        HashAlgorithm::Sha384,
        HashAlgorithm::Sha512,
        HashAlgorithm::Sha3_256,
        HashAlgorithm::Sha3_512,
    ] {
        composite::check_digest(long).unwrap();
    }

    let signer = generated().unlock(&Password::empty()).unwrap();
    let digest = [1_u8; 28];
    assert!(
        matches!(
            signer
                .secret_key()
                .sign_digest(HashAlgorithm::Sha224, &digest),
            Err(Error::DigestTooShort { .. })
        ),
        "signing refuses SHA2-224"
    );
    let through_rpgp = config(
        SignatureType::Binary,
        HashAlgorithm::Sha224,
        vec![created_now()],
    )
    .sign(signer.secret_key(), &Password::empty(), &b"data"[..]);
    assert!(
        through_rpgp.is_err(),
        "rPGP's signing path reaches the same refusal"
    );

    let signature = signer.secret_key().sign_digest(HASH, &[1_u8; 64]).unwrap();
    assert!(
        matches!(
            signer.certificate().public_key().verify_digest(
                HashAlgorithm::Sha224,
                &digest,
                &signature
            ),
            Err(Error::DigestTooShort { .. })
        ),
        "verification refuses SHA2-224"
    );
    let packet = unsigned(config(
        SignatureType::Binary,
        HashAlgorithm::Sha224,
        vec![created_now()],
    ));
    assert!(
        matches!(
            signer.certificate().verify(&packet, &b"data"[..]),
            Err(Error::DigestTooShort { .. })
        ),
        "a SHA2-224 signature packet is refused"
    );
}

#[test]
fn each_half_of_the_composite_signature_must_verify() {
    let signer = a3_key().unlock(&Password::empty()).unwrap();
    let public = signer.certificate().public_key();
    let digest = [9_u8; 64];
    let signature = signer.secret_key().sign_digest(HASH, &digest).unwrap();
    public.verify_digest(HASH, &digest, &signature).unwrap();

    let mut ed25519 = signature.clone();
    ed25519[10] ^= 1;
    assert!(
        matches!(
            public.verify_digest(HASH, &digest, &ed25519),
            Err(Error::Ed25519Invalid)
        ),
        "a changed Ed25519 half"
    );
    let mut ml_dsa = signature.clone();
    ml_dsa[1000] ^= 1;
    assert!(
        matches!(
            public.verify_digest(HASH, &digest, &ml_dsa),
            Err(Error::MlDsaInvalid)
        ),
        "a changed ML-DSA-65 half"
    );
    assert!(
        matches!(
            public.verify_digest(HASH, &digest, &signature[..SIGNATURE_LEN - 1]),
            Err(Error::SignatureLength { found }) if found == SIGNATURE_LEN - 1
        ),
        "a short signature"
    );
    let other = generated();
    assert!(
        other
            .certificate()
            .public_key()
            .verify_digest(HASH, &digest, &signature)
            .is_err(),
        "another key"
    );
}

#[test]
fn keys_of_another_algorithm_are_refused_before_any_rpgp_cryptography() {
    let ed25519 = rpgp_key(KeyType::Ed25519, KeyVersion::V6);
    assert!(
        matches!(
            Key::from_signed(ed25519.clone()),
            Err(Error::Algorithm { .. })
        ),
        "a v6 Ed25519 key"
    );
    assert!(
        matches!(
            Certificate::from_signed(ed25519.to_public_key()),
            Err(Error::Algorithm { .. })
        ),
        "a v6 Ed25519 certificate"
    );
    let legacy = rpgp_key(KeyType::Ed25519Legacy, KeyVersion::V4);
    assert!(
        matches!(
            Certificate::from_signed(legacy.to_public_key()),
            Err(Error::Algorithm { .. })
        ),
        "a v4 EdDSA certificate"
    );

    // A signature made by rPGP's Ed25519 code is refused on its algorithm,
    // before any hashing or verification runs.
    let mut config = SignatureConfig::v6(
        OsRng,
        SignatureType::Binary,
        ed25519.primary_key.algorithm(),
        HashAlgorithm::Sha256,
    )
    .unwrap();
    config.hashed_subpackets = vec![created_now()];
    let classical = config
        .sign(&ed25519.primary_key, &Password::empty(), &b"data"[..])
        .unwrap();
    assert!(
        matches!(
            generated().certificate().verify(&classical, &b"data"[..]),
            Err(Error::Algorithm { .. })
        ),
        "an Ed25519 signature against an ML-DSA-65+Ed25519 certificate"
    );
}

#[test]
fn an_algorithm_30_key_must_be_version_6_with_the_fixed_material_length() {
    let material = a3_certificate()
        .public_key()
        .packet()
        .public_params()
        .clone();
    let v4 = PubKeyInner::new(
        KeyVersion::V4,
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        Timestamp::now(),
        None,
        material,
    )
    .and_then(PublicKey::from_inner)
    .unwrap();
    assert!(
        matches!(
            composite::CompositePublicKey::try_from(v4),
            Err(Error::KeyVersion {
                found: KeyVersion::V4
            })
        ),
        "RFC 9980 §3.5 binds algorithm 30 to v6 keys"
    );
    let short = PublicParams::try_from_reader(
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        Some(100),
        &[0_u8; 100][..],
    )
    .unwrap();
    let packet = PubKeyInner::new(
        KeyVersion::V6,
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        Timestamp::now(),
        None,
        short,
    )
    .and_then(PublicKey::from_inner)
    .unwrap();
    assert!(
        matches!(
            composite::CompositePublicKey::try_from(packet),
            Err(Error::KeyMaterial { found: 100, .. })
        ),
        "1984 octets of public key material"
    );
}

#[test]
fn secret_material_must_belong_to_its_public_key() {
    let a3 = a3_certificate();
    let other = generated();
    let material = a3_key()
        .unlock(&Password::empty())
        .map(|signer| signer.secret_key().public().clone())
        .unwrap();
    assert_eq!(&material, a3.public_key(), "the A.3 key");
    let mut seeds = Vec::new();
    a3_key()
        .signed()
        .primary_key
        .unlock(&Password::empty(), |_, plain| {
            plain.to_writer(&mut seeds, KeyVersion::V6)
        })
        .unwrap()
        .unwrap();
    assert_eq!(seeds.len(), 64, "RFC 9980 §5.3.2.2: 32 + 32 octets");
    CompositeSecretKey::from_material(a3.public_key().clone(), &seeds).unwrap();
    assert!(
        matches!(
            CompositeSecretKey::from_material(other.certificate().public_key().clone(), &seeds),
            Err(Error::KeyMismatch)
        ),
        "the A.3 seeds against another key"
    );
    assert!(
        matches!(
            CompositeSecretKey::from_material(a3.public_key().clone(), &seeds[..63]),
            Err(Error::KeyMaterial { found: 63, .. })
        ),
        "63 octets"
    );
}

#[test]
fn a_signature_of_the_wrong_form_is_refused() {
    let key = generated();
    let certificate = key.certificate();
    let issuer =
        |fingerprint| Subpacket::regular(SubpacketData::IssuerFingerprint(fingerprint)).unwrap();

    let not_a_document = unsigned(config(SignatureType::Key, HASH, vec![created_now()]));
    assert!(
        matches!(
            certificate.verify(&not_a_document, &b""[..]),
            Err(Error::SignatureType { .. })
        ),
        "a direct-key signature is no detached signature"
    );
    let no_time = unsigned(config(SignatureType::Binary, HASH, Vec::new()));
    assert!(
        matches!(
            certificate.verify(&no_time, &b""[..]),
            Err(Error::NoCreationTime)
        ),
        "no creation time"
    );
    let other_issuer = unsigned(config(
        SignatureType::Binary,
        HASH,
        vec![created_now(), issuer(generated().fingerprint())],
    ));
    assert!(
        matches!(
            certificate.verify(&other_issuer, &b""[..]),
            Err(Error::Issuer)
        ),
        "another issuer"
    );
    let before_the_key = unsigned(config(
        SignatureType::Binary,
        HASH,
        vec![
            Subpacket::regular(SubpacketData::SignatureCreationTime(Timestamp::from_secs(
                1,
            )))
            .unwrap(),
        ],
    ));
    assert!(
        matches!(
            certificate.verify(&before_the_key, &b""[..]),
            Err(Error::OutsideValidity)
        ),
        "made before the key existed"
    );
    let mut v4 = SignatureConfig::v4(
        SignatureType::Binary,
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        HASH,
    );
    v4.hashed_subpackets = vec![created_now()];
    assert!(
        matches!(
            certificate.verify(&unsigned(v4), &b""[..]),
            Err(Error::SignatureVersion { .. })
        ),
        "a v4 signature (RFC 9980 §5.3.1)"
    );
}

/// Re-signs the certificate's details with `direct` and `revocations`.
fn with_self_signatures(
    key: &Key,
    direct: Vec<Signature>,
    revocations: Vec<Signature>,
) -> SignedPublicKey {
    let mut signed = key.certificate().signed().clone();
    signed.details.direct_signatures = direct;
    signed.details.revocation_signatures = revocations;
    signed
}

fn self_signature(key: &Key, typ: SignatureType, extra: Vec<Subpacket>) -> Signature {
    let signer = key.unlock(&Password::empty()).unwrap();
    let mut subpackets = vec![created_now()];
    subpackets.extend(extra);
    config(typ, HASH, subpackets)
        .sign_key(
            signer.secret_key(),
            &Password::empty(),
            signer.certificate().public_key(),
        )
        .unwrap()
}

#[test]
fn a_certificate_that_fails_its_checks_is_refused() {
    let key = generated();
    let valid_direct = key.certificate().signed().details.direct_signatures.clone();

    assert!(
        matches!(
            Certificate::from_signed(with_self_signatures(&key, Vec::new(), Vec::new())),
            Err(Error::NoDirectKeySignature)
        ),
        "no direct-key self-signature (RFC 9580 §5.2.3.10)"
    );
    let forged = unsigned(config(SignatureType::Key, HASH, vec![created_now()]));
    assert!(
        matches!(
            Certificate::from_signed(with_self_signatures(&key, vec![forged], Vec::new())),
            Err(Error::NoDirectKeySignature)
        ),
        "a direct-key signature that does not verify"
    );

    let mut certify_only = KeyFlags::default();
    certify_only.set_certify(true);
    let no_sign = self_signature(
        &key,
        SignatureType::Key,
        vec![Subpacket::regular(SubpacketData::KeyFlags(certify_only)).unwrap()],
    );
    assert!(
        matches!(
            Certificate::from_signed(with_self_signatures(&key, vec![no_sign], Vec::new())),
            Err(Error::CannotSign)
        ),
        "no signing flag"
    );

    let revocation = self_signature(&key, SignatureType::KeyRevocation, Vec::new());
    assert!(
        matches!(
            Certificate::from_signed(with_self_signatures(&key, valid_direct, vec![revocation])),
            Err(Error::Revoked)
        ),
        "a key revocation"
    );
}

#[test]
fn a_key_expiration_time_bounds_the_validity() {
    let key = generated();
    let mut flags = KeyFlags::default();
    flags.set_sign(true);
    let expiring = self_signature(
        &key,
        SignatureType::Key,
        vec![
            Subpacket::regular(SubpacketData::KeyFlags(flags)).unwrap(),
            Subpacket::regular(SubpacketData::KeyExpirationTime(Duration::from_secs(60))).unwrap(),
        ],
    );
    let certificate =
        Certificate::from_signed(with_self_signatures(&key, vec![expiring], Vec::new())).unwrap();
    let created = certificate.public_key().created_at().as_secs();
    assert_eq!(
        certificate.expires(),
        Some(Timestamp::from_secs(created + 60)),
        "creation plus the lifetime"
    );
    assert!(
        certificate.valid_at(Timestamp::from_secs(created + 59)),
        "inside"
    );
    assert!(
        !certificate.valid_at(Timestamp::from_secs(created + 60)),
        "at expiry"
    );
    assert!(
        !certificate.valid_at(Timestamp::from_secs(created - 1)),
        "before creation"
    );
}

#[test]
fn the_crate_names_no_rpgp_public_key_primitive() {
    // NOTE: the OpenVEX statements for RUSTSEC-2023-0071 and RUSTSEC-2026-0387 rest on this: every public-key operation runs in aws-lc-rs.
    let sources = [
        include_str!("../../src/composite.rs"),
        include_str!("../../src/certificate.rs"),
        include_str!("../../src/key.rs"),
        include_str!("../../src/signature.rs"),
        include_str!("../../src/encoding.rs"),
        include_str!("../../src/mode.rs"),
        include_str!("../../src/error.rs"),
        include_str!("../../src/lib.rs"),
        include_str!("../../src/main.rs"),
        include_str!("../../src/sop.rs"),
        include_str!("../../src/sop/commands.rs"),
        include_str!("../../src/sop/date.rs"),
        include_str!("../../src/sop/exit.rs"),
        include_str!("../../src/sop/input.rs"),
    ];
    for source in sources {
        for line in source.lines().filter(|line| line.contains("pgp::crypto::")) {
            assert!(
                line.contains("pgp::crypto::hash::HashAlgorithm")
                    || line.contains("pgp::crypto::public_key::PublicKeyAlgorithm"),
                "only the algorithm identifiers of pgp::crypto are named: {line}"
            );
        }
        for primitive in [
            "rsa",
            "ecdsa",
            "ed448",
            "elliptic_curve",
            "crypto_bigint",
            "x448",
            "p256",
            "p384",
            "p521",
            "k256",
        ] {
            assert!(
                !source.contains(&format!("{primitive}::")),
                "no {primitive} item is named"
            );
        }
    }
}
