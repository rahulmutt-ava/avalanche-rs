// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 20-byte `ShortId` newtype (addresses).
//!
//! Mirrors Go `ids.ShortID`. The derived [`Ord`] is lexicographic over the
//! byte array (== Go `bytes.Compare`).
//!
//! TODO(M0.6): `Display`/`FromStr`/serde CB58 string forms, which depend on the
//! CB58 codec being built in `ava-utils`.
//! Owning spec: `specs/03-core-primitives.md` §1.1.

use crate::error::{Error, Result};

/// Length of a [`ShortId`] in bytes.
pub const SHORT_ID_LEN: usize = 20;

/// 20-byte identifier (addresses). Mirrors `ids.ShortID`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ShortId([u8; SHORT_ID_LEN]);

impl ShortId {
    /// The all-zero short id.
    pub const EMPTY: ShortId = ShortId([0u8; SHORT_ID_LEN]);

    /// Constructs a [`ShortId`] from a byte slice.
    ///
    /// # Errors
    /// Returns [`Error::InvalidHashLen`] if `bytes.len() != 20`
    /// (mirrors Go `hashing.ToHash160`).
    pub fn from_slice(bytes: &[u8]) -> Result<ShortId> {
        if bytes.len() != SHORT_ID_LEN {
            return Err(Error::InvalidHashLen {
                expected: SHORT_ID_LEN,
                actual: bytes.len(),
            });
        }
        let mut out = [0u8; SHORT_ID_LEN];
        out.copy_from_slice(bytes);
        Ok(ShortId(out))
    }

    /// Returns a reference to the raw 20 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SHORT_ID_LEN] {
        &self.0
    }

    /// Consumes the short id, returning the raw 20 bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; SHORT_ID_LEN] {
        self.0
    }

    /// Lowercase hex, no `0x` prefix.
    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl core::fmt::Debug for ShortId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // TODO(M0.6): use CB58 Display once available.
        write!(f, "ShortId(0x{})", self.hex())
    }
}

impl From<[u8; SHORT_ID_LEN]> for ShortId {
    fn from(bytes: [u8; SHORT_ID_LEN]) -> ShortId {
        ShortId(bytes)
    }
}
