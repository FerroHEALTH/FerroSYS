<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Changelog of ferrohealth-openpgp-pq

Every change a product sees is recorded here, following
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/). The crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html) and is
released by the tag `ferrohealth-openpgp-pq-vX.Y.Z`.

## [Unreleased]

### Added

- ML-DSA-65+Ed25519 OpenPGP signatures (RFC 9980 algorithm ID 30) on RFC
  9580 version 6 keys and signatures. rPGP 0.21 reads and writes the packets
  with its `pqc` feature off, and aws-lc-rs computes the Ed25519 half and
  the hedged ML-DSA-65 half over the same dataDigest. Verification requires
  both halves, and signing and verification refuse a hash under 256 bits.
  `key::Key` generates, reads, locks and unlocks a key;
  `certificate::Certificate` checks a certificate and verifies detached
  signatures; `composite` implements rPGP's `KeyDetails`, `SigningKey` and
  `VerifyingKey`. The RFC 9980 Appendix A.3 vectors are part of the tests
  (#438).
- The `ferrohealth-sop` binary (feature `cli`, on by default): `version`,
  `list-profiles`, `generate-key`, `extract-cert`, `sign` and `verify` of
  the Stateless OpenPGP command line, draft-dkg-openpgp-stateless-cli-16,
  with its `@ENV:` and `@FD:` inputs and its return codes (#438).

### Security

Shipped with functional changes because: this is the first release, and
the statements describe the dependencies it ships with.

- Two advisories in rPGP's dependencies that this crate never executes are
  accepted, each with an OpenVEX statement: RUSTSEC-2023-0071 in `rsa`
  (`security/vex/rust-advisories.openvex.json`) and RUSTSEC-2026-0387 in
  `crypto-bigint` (`security/vex/rust-advisories.openvex.json`). The crate
  signs and verifies only with ML-DSA-65+Ed25519 in aws-lc-rs and refuses
  every other algorithm before it is used.
