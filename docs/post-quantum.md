<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Post-quantum inventory

This page lists every cryptographic use in the FerroSYS crates and their
release workflows, the algorithm each uses, and whether it resists a
cryptographically relevant quantum computer. The machine-readable copy is
`conformance/post-quantum.tsv`, one row per use, with five tab-separated
columns: `use`, `algorithm`, `safe` (`yes` or `no`), `pq_standard` (the
standard that defines the post-quantum form, or `no standard yet`) and
`location`. Every change that adds, removes or changes a cryptographic use
updates both in the same pull request. `scripts/checks/post-quantum.sh
--update` rewrites the table below and the README badge
(`conformance/badges/post-quantum.json`) from the file, and CI fails when
either disagrees with it or a location names a file the tree does not hold.

## The sources

- **NIST IR 8547 (final):** ECDSA, EdDSA and RSA signatures (§4.1.1) and
  ECDH, DH and RSA key establishment (§4.1.2) are deprecated after 2030 and
  disallowed after 2035. Symmetric primitives with at least 128 bits of
  classical security, AES-128 and up, SHA-256 and HMAC-SHA-256 among them,
  stay (§4.1.3); the 112-bit ones are disallowed in 2030. FIPS 203, 204 and
  205 define ML-KEM, ML-DSA and SLH-DSA.
- **The EU coordinated implementation roadmap (NIS Cooperation Group,
  v1.1):** a use case is high-risk when its confidentiality must hold for
  10 years or more, which health records need, and high-risk use cases
  complete the transition by 31.12.2030 (§4.2). After 2030 they use no
  quantum-vulnerable public-key mechanism stand-alone, and standardised
  hybrid solutions are recommended (§4.1).
- **RFC 10024:** the TLS 1.3 hybrid group X25519MLKEM768 (codepoint
  0x11EC), within the hybrid design of RFC 9954.
- **RFC 9980:** post-quantum cryptography in OpenPGP; ML-DSA-65+Ed25519
  (algorithm ID 30) is the signature every implementation must support.
- **RFC 9964:** ML-DSA for JOSE and COSE. **RFC 9881:** ML-DSA in X.509,
  CRLs and OCSP.

A row is `safe` when every key it depends on is established or verified by
a post-quantum or hybrid mechanism, or when it is a symmetric primitive at
128 bits or more. A row whose post-quantum form has no standard yet counts
as not safe.

## What FerroSYS does

The listener and every client this crate configures (the healthcheck,
`TlsClient::client_config`, `TlsClient::platform_config` and the revocation
link) offer X25519MLKEM768 first in the TLS 1.3 key exchange, then X25519,
P-256 and P-384 for a peer that lacks the hybrid group. The crates enable
rustls's `prefer-post-quantum` feature, so every other rustls user in a
product that links `ferrohealth-server`, the reqwest client
`TlsClient::apply` configures and the database drivers included, takes the
same order from the default provider.

The key exchange protects what a session carries against an attacker who
records it now and decrypts it later, the risk the roadmap gives first
place. The certificate signatures that authenticate a peer stay classical:
no TLS 1.3 signature scheme for ML-DSA is published as an RFC yet, and the
operator's certificates decide the algorithm.

## The inventory

| Use | Algorithm | Safe | Post-quantum standard | Location |
|---|---|---|---|---|
| TLS 1.3 key exchange of the listener | X25519MLKEM768 first; X25519, P-256, P-384 for a peer without it | yes | RFC 10024 | `crates/ferrohealth-server/src/tls.rs` |
| TLS 1.3 key exchange of the healthcheck, TlsClient and revocation clients | X25519MLKEM768 first; X25519, P-256, P-384 for a peer without it | yes | RFC 10024 | `crates/ferrohealth-server/src/tls.rs, crates/ferrohealth-server/src/tls_client.rs, crates/ferrohealth-server/src/revocation.rs` |
| TLS 1.3 key exchange of reqwest clients and other default-provider rustls users | X25519MLKEM768 first (rustls prefer-post-quantum) | yes | RFC 10024 | `Cargo.toml (rustls features), crates/ferrohealth-server/src/tls_client.rs (TlsClient::apply)` |
| TLS 1.3 key exchange of the OTLP export | X25519MLKEM768 first, through the TlsClient configuration | yes | RFC 10024 | `crates/ferrohealth-telemetry/src/telemetry/otlp.rs` |
| TLS 1.2 key exchange where min_version lowers the minimum | ECDHE over X25519, P-256 or P-384 | no | no standard yet | `crates/ferrohealth-server/src/tls.rs (protocol_versions)` |
| TLS server and client certificate authentication | ECDSA, RSA or Ed25519, as the operator's certificates hold | no | no standard yet | `crates/ferrohealth-server/src/tls.rs, crates/ferrohealth-server/src/tls_client.rs` |
| TLS record protection | AES-128-GCM, AES-256-GCM, ChaCha20-Poly1305 | yes | NIST IR 8547 4.1.3 | `crates/ferrohealth-server/src/tls.rs` |
| CRL and OCSP response signature verification | RSA, ECDSA, Ed25519 | no | RFC 9881 | `crates/ferrohealth-server/src/revocation/signature.rs` |
| OCSP CertID and certificate digests | SHA-1 (RFC 6960 CertID), SHA-256 | yes | NIST IR 8547 4.1.3 | `crates/ferrohealth-server/src/revocation/ocsp.rs, crates/ferrohealth-server/src/revocation/chain.rs` |
| OpenPGP signatures of ferrohealth-openpgp-pq | ML-DSA-65+Ed25519 (RFC 9980 algorithm ID 30) over SHA2-512, v6 keys and signatures | yes | RFC 9980 | `crates/ferrohealth-openpgp-pq/src/composite.rs` |
| Password protection of a ferrohealth-openpgp-pq secret key | AEAD with an Argon2 derived key (RFC 9580 v6 S2K) | yes | NIST IR 8547 4.1.3 | `crates/ferrohealth-openpgp-pq/src/key.rs` |
| DPoP proofs and client assertions the performance gate signs | ES256 (ECDSA P-256) | no | RFC 9964 | `crates/ferrohealth-perf/src/credentials.rs` |
| crates.io publishing | Trusted Publishing (OIDC token from GitHub, RS256) | no | no standard yet | `.github/workflows/release.yml, .github/workflows/publish-crates.yml` |
| Release build provenance | Sigstore attestation (ECDSA P-256) | no | no standard yet | `.github/workflows/release-crate.yml` |
