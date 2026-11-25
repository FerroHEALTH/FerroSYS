// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The RFC 9980 Appendix A.3 vectors and the keys the cases share.

use ferrohealth_openpgp_pq::certificate::Certificate;
use ferrohealth_openpgp_pq::key::Key;

/// The A.3.1 transferable secret key.
pub(crate) const A3_SECRET_KEY: &str = include_str!("../vectors/rfc9980-a3.1-secret-key.asc");
/// The A.3.2 transferable public key.
pub(crate) const A3_CERTIFICATE: &str = include_str!("../vectors/rfc9980-a3.2-certificate.asc");
/// The A.3.4 detached signature over [`A3_MESSAGE`].
pub(crate) const A3_SIGNATURE: &str = include_str!("../vectors/rfc9980-a3.4-signature.asc");
/// The message A.3.4 signs.
pub(crate) const A3_MESSAGE: &[u8] = b"Testing\n";
/// The A.3.2 primary key fingerprint RFC 9980 states.
pub(crate) const A3_FINGERPRINT: &str =
    "a3e2e14b6a493ff930fb27321f125e9a6880338be9fb7da3ae065ea65793242f";

pub(crate) fn a3_certificate() -> Certificate {
    Certificate::read(A3_CERTIFICATE.as_bytes()).expect("the A.3.2 certificate should read")
}

pub(crate) fn a3_key() -> Key {
    Key::read(A3_SECRET_KEY.as_bytes()).expect("the A.3.1 key should read")
}

pub(crate) fn generated() -> Key {
    Key::generate(&["Alice <alice@example.org>"]).expect("a key should generate")
}
