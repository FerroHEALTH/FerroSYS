// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Post-quantum OpenPGP signatures for the FerroHEALTH products.
//!
//! The crate signs and verifies with the composite ML-DSA-65+Ed25519
//! algorithm, public-key algorithm ID 30 of RFC 9980, which every
//! implementation of that specification must support (RFC 9980 §2.1). Keys
//! and signatures are RFC 9580 version 6 packets, the only version RFC 9980
//! allows for it (§3.5, §5.3).
//!
//! [rPGP](https://docs.rs/pgp/0.21.1) reads and writes the packets with its
//! `pqc` feature off; [aws-lc-rs](https://docs.rs/aws-lc-rs/1.19.0) computes
//! both component signatures over the same dataDigest (RFC 9980 §5.2.2), the
//! Ed25519 one and the hedged ML-DSA-65 one. Verification requires both to
//! verify (RFC 9980 §3.2), and signing and verification both refuse a hash
//! whose digest is shorter than 256 bits (RFC 9980 §5.3.1).
//!
//! - [`key::Key`] is a Transferable Secret Key (RFC 9580 §10.2): generated,
//!   read, locked with a password and unlocked into a [`key::Signer`].
//! - [`certificate::Certificate`] is a validated Transferable Public Key
//!   (RFC 9580 §10.1.1) that verifies detached signatures.
//! - [`composite`] holds the key material and the two component operations,
//!   and implements rPGP's `KeyDetails`, `SigningKey` and `VerifyingKey`
//!   traits.
//!
//! Only the primary key signs: a key this crate generates has no subkeys,
//! and a certificate's subkeys are never used to verify.

pub mod certificate;
pub mod composite;
pub mod encoding;
pub mod error;
pub mod key;
pub mod mode;
pub mod signature;
