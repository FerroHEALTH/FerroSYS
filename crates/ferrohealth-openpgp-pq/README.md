<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# ferrohealth-openpgp-pq

Post-quantum OpenPGP signatures for the FerroHEALTH products, part of
[FerroSYS](https://github.com/FerroHEALTH/FerroSYS). The crate signs and
verifies with ML-DSA-65+Ed25519, the composite algorithm (ID 30) that RFC
9980 requires every implementation to support, on RFC 9580 version 6 keys
and signatures. A signature stays valid only while both its Ed25519 half and
its ML-DSA-65 half verify, so it holds against a classical attacker today
and a quantum one later.

## What it does

- `key::Key` is a Transferable Secret Key. `Key::generate` makes a v6
  primary key that may certify and sign, with a direct-key self-signature
  and a positive certification of each User ID; the key has no subkeys.
  `lock` protects it with the version 6 S2K of RFC 9580 (AEAD with an
  Argon2 key), and `unlock` gives a `Signer`.
- `key::Signer::sign` makes a v6 detached signature with SHA2-512, in
  binary or canonical text mode (`mode::Mode`).
- `certificate::Certificate` reads a certificate and checks it: a v6
  algorithm 30 primary key, a valid direct-key self-signature with the
  signing flag, no valid revocation. `Certificate::verify` checks a detached
  signature's form before hashing (v6, binary or text, algorithm 30, a hash
  of 256 bits or more, a creation time inside the key's validity) and then
  both halves.
- `composite` holds the key material and implements rPGP's `KeyDetails`,
  `SigningKey` and `VerifyingKey`, so rPGP's own APIs sign and verify with
  it.
- `encoding::encode` writes keys, certificates and signatures armored, with
  no CRC24 footer (RFC 9580 §6.1), or binary.

Both halves run in [aws-lc-rs](https://docs.rs/aws-lc-rs): Ed25519 as
PureEdDSA, and ML-DSA-65 in its default hedged form with an empty context
(RFC 9980 §5.1). The secret key is stored as RFC 9980 §5.3.2.2 gives it,
the Ed25519 secret and the 32-octet ML-DSA seed. [rPGP](https://docs.rs/pgp)
reads and writes the packets with its `pqc` feature off, and runs no
public-key operation on this crate's keys.

## Use

```toml
[dependencies]
ferrohealth-openpgp-pq = { version = "=0.1.0", default-features = false }
```

```rust
use ferrohealth_openpgp_pq::key::Key;
use ferrohealth_openpgp_pq::mode::Mode;
use pgp::types::Password;

fn sign_and_check() -> Result<(), ferrohealth_openpgp_pq::error::Error> {
    let key = Key::generate(&["Records <records@example.org>"])?;
    let signer = key.unlock(&Password::empty())?;
    let signature = signer.sign(&b"canonical form"[..], Mode::Binary)?;
    key.certificate().verify(&signature.signature, &b"canonical form"[..])?;
    Ok(())
}
```

Pin the version exactly, as every FerroHEALTH product does. A library user
turns default features off, which leaves out the command line.

## The command line

`ferrohealth-sop` implements a subset of the Stateless OpenPGP command line,
[draft-dkg-openpgp-stateless-cli-16](https://datatracker.ietf.org/doc/draft-dkg-openpgp-stateless-cli/16/),
an Internet-Draft, for ML-DSA-65+Ed25519 keys only:

```sh
cargo install ferrohealth-openpgp-pq --version =0.1.0 --locked
```

| Subcommand | Reads | Writes |
|---|---|---|
| `version [--backend\|--extended\|--sop-spec]` | | the version |
| `list-profiles generate-key` | | the one profile, `default` |
| `generate-key [--no-armor] [--with-key-password=PASSWORD] [--profile=default] [--signing-only] [USERID...]` | | KEYS |
| `extract-cert [--no-armor]` | KEYS | CERTS |
| `sign [--no-armor] [--as=binary\|text] [--with-key-password=PASSWORD...] KEYS...` | DATA | SIGNATURES |
| `verify [--not-before=DATE] [--not-after=DATE] SIGNATURES CERTS...` | DATA | VERIFICATIONS |

KEYS, CERTS, SIGNATURES and PASSWORD are file names, `@ENV:NAME` or
`@FD:n`; input may be armored or binary, and output is armored unless
`--no-armor` is given. `verify` writes one line per valid signature, as
`2026-11-25T10:00:00Z <FINGERPRINT> <FINGERPRINT> mode:binary
{"signers":["cert.asc"]}`, and exits 3 when none verifies. A failure exits
with the code of the draft's §8 and its reason on standard error: 13 for a
key or certificate of another algorithm, 19 for a missing argument, 37 for
an option or value it does not handle, 41 for input of the wrong kind, 53
for non-text input to `--as=text`, 61 for a missing file, 67 for a key no
password unlocks, 69 for a subcommand it does not offer, 71 and 73 for a
designator it cannot use, 79 for a key that may not sign, 83 for options
that exclude each other, 89 for an unknown profile. A generated key signs
and does not decrypt.

## Security

`pgp` 0.21 links `rsa` 0.9 (RUSTSEC-2023-0071) and `crypto-bigint` 0.5
(RUSTSEC-2026-0387) whether or not they run. This crate runs neither: it
refuses every key and signature of another algorithm before using it, and
computes both halves in aws-lc-rs. FerroSYS accepts the two advisories with
OpenVEX statements in
[`security/vex/`](https://github.com/FerroHEALTH/FerroSYS/tree/main/security/vex),
which name the rPGP release that ends each.

## Specifications

- [RFC 9980](https://www.rfc-editor.org/rfc/rfc9980.html), post-quantum
  cryptography in OpenPGP: §5 (ML-DSA + EdDSA), Appendix A.3 (the vectors
  the tests read).
- [RFC 9580](https://www.rfc-editor.org/rfc/rfc9580.html), OpenPGP.
- draft-dkg-openpgp-stateless-cli-16, the command line.

## Licence

Business Source License 1.1, Licensor Cadasto B.V., Change License Apache
2.0 four years after each version (`LICENSE` and `NOTICE` in the
repository).
