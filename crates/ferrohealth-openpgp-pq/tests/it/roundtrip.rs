// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Generated keys, their certificates and signatures, written and read back,
//! by this crate and by rPGP's own parser.

use ferrohealth_openpgp_pq::certificate::Certificate;
use ferrohealth_openpgp_pq::composite::{self, HASH, SIGNATURE_LEN};
use ferrohealth_openpgp_pq::encoding::{self, Encoding};
use ferrohealth_openpgp_pq::error::Error;
use ferrohealth_openpgp_pq::key::Key;
use ferrohealth_openpgp_pq::mode::Mode;
use ferrohealth_openpgp_pq::signature;
use pgp::composed::{Deserializable as _, DetachedSignature, SignedPublicKey};
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::packet::{SignatureType, SignatureVersion};
use pgp::types::{KeyDetails as _, KeyVersion, Password, SignatureBytes};

use crate::support::generated;

#[test]
fn a_generated_key_is_a_v6_algorithm_30_key_that_may_sign() {
    let key = generated();
    let certificate = key.certificate();
    let primary = certificate.public_key();
    assert_eq!(
        primary.version(),
        KeyVersion::V6,
        "version 6 (RFC 9980 §3.5)"
    );
    assert_eq!(
        primary.algorithm(),
        PublicKeyAlgorithm::from(composite::ALGORITHM_ID),
        "algorithm 30"
    );
    assert_eq!(
        certificate.fingerprint().as_bytes().len(),
        32,
        "v6 fingerprint is SHA-256"
    );
    let direct = &certificate.signed().details.direct_signatures;
    assert_eq!(direct.len(), 1, "one direct-key self-signature");
    assert!(direct[0].key_flags().sign(), "the signing flag");
    assert!(direct[0].key_flags().certify(), "the certification flag");
    assert_eq!(certificate.signed().details.users.len(), 1, "the User ID");
    assert!(certificate.signed().public_subkeys.is_empty(), "no subkeys");
    assert!(!key.is_locked(), "generated unlocked");
}

#[test]
fn a_signature_verifies_and_a_changed_message_does_not() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let detached = signer.sign(&b"canonical form"[..], Mode::Binary).unwrap();
    let signature = &detached.signature;
    assert_eq!(signature.version(), SignatureVersion::V6, "v6 signature");
    assert_eq!(signature.hash_alg(), Some(HASH), "SHA2-512");
    assert_eq!(
        signature.issuer_fingerprint(),
        vec![&key.fingerprint()],
        "issuer"
    );
    let Some(SignatureBytes::Native(bytes)) = signature.signature() else {
        panic!("a composite signature is native octets");
    };
    assert_eq!(bytes.len(), SIGNATURE_LEN, "64 + 3309 octets");

    key.certificate()
        .verify(signature, &b"canonical form"[..])
        .unwrap();
    assert!(
        matches!(
            key.certificate().verify(signature, &b"canonical forM"[..]),
            Err(Error::Invalid(_))
        ),
        "a changed message is refused"
    );
}

#[test]
fn a_text_signature_hashes_line_endings_as_crlf() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let detached = signer
        .sign(&b"line one\nline two\n"[..], Mode::Text)
        .unwrap();
    assert_eq!(
        detached.signature.typ(),
        Some(SignatureType::Text),
        "type 0x01"
    );
    let verification = key
        .certificate()
        .verify(&detached.signature, &b"line one\r\nline two\r\n"[..])
        .unwrap();
    assert_eq!(verification.mode(), Mode::Text, "text mode");
}

#[test]
fn signing_is_hedged() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let digest = [7_u8; 64];
    let first = signer.secret_key().sign_digest(HASH, &digest).unwrap();
    let second = signer.secret_key().sign_digest(HASH, &digest).unwrap();
    assert_eq!(first[..64], second[..64], "Ed25519 is deterministic");
    assert_ne!(
        first[64..],
        second[64..],
        "ML-DSA-65 draws fresh randomness (RFC 9980 §9.3)"
    );
    signer
        .certificate()
        .public_key()
        .verify_digest(HASH, &digest, &first)
        .unwrap();
    signer
        .certificate()
        .public_key()
        .verify_digest(HASH, &digest, &second)
        .unwrap();
}

#[test]
fn keys_certificates_and_signatures_read_back_in_both_encodings() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let detached = signer.sign(&b"data"[..], Mode::Binary).unwrap();
    for encoding in [Encoding::Armored, Encoding::Binary] {
        let key_bytes = key.encode(encoding).unwrap();
        let read = Key::read(&key_bytes).unwrap();
        assert_eq!(read.fingerprint(), key.fingerprint(), "key, {encoding:?}");

        let certificate_bytes = key.certificate().encode(encoding).unwrap();
        let certificate = Certificate::read(&certificate_bytes).unwrap();
        assert_eq!(
            certificate.fingerprint(),
            key.fingerprint(),
            "certificate, {encoding:?}"
        );

        let signature_bytes = encoding::encode(std::slice::from_ref(&detached), encoding).unwrap();
        let signatures = signature::read_all(&signature_bytes).unwrap();
        certificate
            .verify(&signatures[0].signature, &b"data"[..])
            .unwrap();
    }
}

#[test]
fn armor_carries_no_crc24_footer() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let detached = signer.sign(&b"data"[..], Mode::Binary).unwrap();
    let outputs = [
        key.encode(Encoding::Armored).unwrap(),
        key.certificate().encode(Encoding::Armored).unwrap(),
        encoding::encode(&[detached], Encoding::Armored).unwrap(),
    ];
    for output in outputs {
        let text = String::from_utf8(output).unwrap();
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with('=') && line.len() == 5),
            "RFC 9580 §6.1 forbids the CRC24 footer for v6 keys and signatures"
        );
    }
}

#[test]
fn rpgp_parses_what_the_crate_writes() {
    let key = generated();
    let signer = key.unlock(&Password::empty()).unwrap();
    let detached = signer.sign(&b"data"[..], Mode::Binary).unwrap();
    let certificate_bytes = key.certificate().encode(Encoding::Binary).unwrap();
    let (parsed, _) = SignedPublicKey::from_reader_single(&*certificate_bytes).unwrap();
    assert_eq!(
        parsed.primary_key.fingerprint(),
        key.fingerprint(),
        "rPGP fingerprint"
    );
    assert_eq!(&parsed, key.certificate().signed(), "the same packets");
    let signature_bytes =
        encoding::encode(std::slice::from_ref(&detached), Encoding::Binary).unwrap();
    let (parsed, _) = DetachedSignature::from_reader_single(&*signature_bytes).unwrap();
    assert_eq!(parsed, detached, "the same signature packet");
}

#[test]
fn several_signatures_travel_in_one_block() {
    let first = generated();
    let second = generated();
    let signatures = [
        first
            .unlock(&Password::empty())
            .unwrap()
            .sign(&b"data"[..], Mode::Binary)
            .unwrap(),
        second
            .unlock(&Password::empty())
            .unwrap()
            .sign(&b"data"[..], Mode::Binary)
            .unwrap(),
    ];
    let block = encoding::encode(&signatures, Encoding::Armored).unwrap();
    let read = signature::read_all(&block).unwrap();
    assert_eq!(read.len(), 2, "both signatures");
    first
        .certificate()
        .verify(&read[0].signature, &b"data"[..])
        .unwrap();
    second
        .certificate()
        .verify(&read[1].signature, &b"data"[..])
        .unwrap();
}

#[test]
fn a_locked_key_unlocks_only_with_its_password() {
    let mut key = generated();
    key.lock(&Password::from("correct horse")).unwrap();
    assert!(key.is_locked(), "locked");
    assert!(
        matches!(
            key.lock(&Password::from("again")),
            Err(Error::AlreadyLocked)
        ),
        "relock"
    );

    let read = Key::read(&key.encode(Encoding::Armored).unwrap()).unwrap();
    assert!(read.is_locked(), "stays locked on disk");
    assert!(
        matches!(read.unlock(&Password::from("wrong")), Err(Error::Locked(_))),
        "wrong"
    );
    assert!(
        matches!(read.unlock(&Password::empty()), Err(Error::Locked(_))),
        "none"
    );
    let signer = read.unlock(&Password::from("correct horse")).unwrap();
    let detached = signer.sign(&b"data"[..], Mode::Binary).unwrap();
    key.certificate()
        .verify(&detached.signature, &b"data"[..])
        .unwrap();
}
