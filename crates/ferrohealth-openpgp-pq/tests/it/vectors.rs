// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The RFC 9980 Appendix A.3 ML-DSA-65+Ed25519 vectors.

use ferrohealth_openpgp_pq::mode::Mode;
use ferrohealth_openpgp_pq::signature;
use pgp::types::{KeyDetails as _, Password, Tag};

use crate::support::{A3_FINGERPRINT, A3_MESSAGE, A3_SIGNATURE, a3_certificate, a3_key};

#[test]
fn the_a3_certificate_has_the_fingerprint_rfc_9980_states() {
    let certificate = a3_certificate();
    assert_eq!(
        certificate.fingerprint().to_string(),
        A3_FINGERPRINT,
        "A.3 fingerprint"
    );
    assert_eq!(
        certificate.public_key().ed25519().len(),
        32,
        "Ed25519 public key"
    );
    assert_eq!(
        certificate.public_key().ml_dsa_65().len(),
        1952,
        "ML-DSA-65 public key"
    );
}

#[test]
fn the_a3_detached_signature_verifies_over_the_rfc_message() {
    let certificate = a3_certificate();
    let signatures = signature::read_all(A3_SIGNATURE.as_bytes()).unwrap();
    assert_eq!(signatures.len(), 1, "A.3.4 holds one signature");
    let verification = certificate
        .verify(&signatures[0].signature, A3_MESSAGE)
        .unwrap();
    assert_eq!(
        verification.mode(),
        Mode::Text,
        "A.3.4 is a text signature (type 0x01)"
    );
    certificate
        .verify(&signatures[0].signature, &b"Testing\r\n"[..])
        .unwrap();
}

#[test]
fn the_a3_signature_does_not_verify_over_another_message() {
    let certificate = a3_certificate();
    let signatures = signature::read_all(A3_SIGNATURE.as_bytes()).unwrap();
    assert!(
        certificate
            .verify(&signatures[0].signature, &b"Testing!\n"[..])
            .is_err(),
        "a changed message is refused"
    );
}

#[test]
fn the_a3_user_id_certification_verifies_with_the_composite_key() {
    let certificate = a3_certificate();
    let user = &certificate.signed().details.users[0];
    for certification in &user.signatures {
        certification
            .verify_certification(certificate.public_key(), Tag::UserId, &user.id)
            .unwrap();
    }
}

#[test]
fn the_a3_secret_seeds_derive_the_a3_public_key() {
    let key = a3_key();
    let certificate = a3_certificate();
    assert_eq!(
        key.fingerprint(),
        certificate.fingerprint(),
        "same primary key"
    );
    let signer = key.unlock(&Password::empty()).unwrap();
    assert_eq!(
        signer.secret_key().public(),
        certificate.public_key(),
        "the seeds expand to the A.3.2 key material"
    );
}

#[test]
fn a_signature_by_the_a3_key_verifies_against_the_a3_certificate() {
    let signer = a3_key().unlock(&Password::empty()).unwrap();
    let detached = signer
        .sign(&b"a version signature"[..], Mode::Binary)
        .unwrap();
    let verification = a3_certificate()
        .verify(&detached.signature, &b"a version signature"[..])
        .unwrap();
    assert_eq!(verification.mode(), Mode::Binary, "binary mode");
    assert!(
        verification.created() >= a3_certificate().public_key().created_at(),
        "made after the key"
    );
}
