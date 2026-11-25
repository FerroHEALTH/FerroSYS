// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! ASCII armor or binary packets on output.

use std::io;

use pgp::armor::{self, BlockType};
use pgp::composed::{DetachedSignature, SignedPublicKey, SignedSecretKey};
use pgp::ser::Serialize;

use crate::error::Error;

/// How keys, certificates and signatures are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// ASCII armor (RFC 9580 §6) with no CRC24 footer, which RFC 9580 §6.1
    /// forbids for version 6 signatures and keys.
    #[default]
    Armored,
    /// The binary packets.
    Binary,
}

/// An OpenPGP object with its ASCII armor block type (RFC 9580 §6.2).
pub trait Block: Serialize {
    /// The armor header line's block type.
    const TYPE: BlockType;
}

impl Block for DetachedSignature {
    const TYPE: BlockType = BlockType::Signature;
}

impl Block for SignedPublicKey {
    const TYPE: BlockType = BlockType::PublicKey;
}

impl Block for SignedSecretKey {
    const TYPE: BlockType = BlockType::PrivateKey;
}

/// Writes `items` one after another as one block: several signatures, or a
/// keyring of several certificates or keys.
///
/// # Errors
///
/// [`Error::Packet`] when an item cannot be serialized.
pub fn encode<T: Block>(items: &[T], encoding: Encoding) -> Result<Vec<u8>, Error> {
    let sequence = Sequence(items);
    match encoding {
        Encoding::Binary => Ok(sequence.to_bytes()?),
        Encoding::Armored => {
            let mut out = Vec::new();
            armor::write(&sequence, T::TYPE, &mut out, None, false)?;
            Ok(out)
        }
    }
}

/// Items serialized back to back.
struct Sequence<'a, T>(&'a [T]);

impl<T: Serialize> Serialize for Sequence<'_, T> {
    fn to_writer<W: io::Write>(&self, writer: &mut W) -> pgp::errors::Result<()> {
        self.0.iter().try_for_each(|item| item.to_writer(writer))
    }

    fn write_len(&self) -> usize {
        self.0.iter().map(Serialize::write_len).sum()
    }
}
