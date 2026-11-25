// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! Whether a detached signature covers bytes or canonical text.

use pgp::packet::SignatureType;

/// What a detached signature covers (RFC 9580 §5.2.1.1, §5.2.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// A signature of a binary document (type 0x00).
    #[default]
    Binary,
    /// A signature of a canonical text document (type 0x01): line endings
    /// are hashed as CR LF.
    Text,
}

impl Mode {
    /// The signature type this mode signs with.
    #[must_use]
    pub fn signature_type(self) -> SignatureType {
        match self {
            Self::Binary => SignatureType::Binary,
            Self::Text => SignatureType::Text,
        }
    }

    /// The mode of a signature type, if it is one of the two document types.
    #[must_use]
    pub fn of(typ: SignatureType) -> Option<Self> {
        match typ {
            SignatureType::Binary => Some(Self::Binary),
            SignatureType::Text => Some(Self::Text),
            _ => None,
        }
    }
}
