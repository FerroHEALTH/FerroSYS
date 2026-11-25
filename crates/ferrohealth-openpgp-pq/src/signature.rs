// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Reading and writing detached signatures.

use pgp::composed::{Deserializable as _, DetachedSignature};

use crate::error::Error;

/// Reads every signature packet in `input`, ASCII-armored or binary.
///
/// # Errors
///
/// [`Error::Packet`] when the input is not OpenPGP signature packets, and
/// [`Error::Missing`] when it holds none.
pub fn read_all(input: &[u8]) -> Result<Vec<DetachedSignature>, Error> {
    let (signatures, _headers) = DetachedSignature::from_reader_many(input)?;
    let signatures = signatures.collect::<Result<Vec<_>, _>>()?;
    if signatures.is_empty() {
        return Err(Error::Missing {
            expected: "signature",
        });
    }
    Ok(signatures)
}
