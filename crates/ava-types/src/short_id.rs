// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 20-byte `ShortId` newtype (addresses).
//!
//! Mirrors Go `ids.ShortID`. The derived [`Ord`] is lexicographic over the
//! byte array (== Go `bytes.Compare`).
//!
//! `Display`/`FromStr` use bare CB58 (no prefix); serde serializes as the
//! quoted Display string; JSON `null` deserializes to `Default` (Go null no-op,
//! spec §1.1). CB58 lives in `ava-utils::cb58` to break the types→crypto cycle.
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

impl core::fmt::Display for ShortId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = ava_utils::cb58::cb58_encode(&self.0).map_err(|_| core::fmt::Error)?;
        f.write_str(&s)
    }
}

impl core::str::FromStr for ShortId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = ava_utils::cb58::cb58_decode(s).map_err(Error::Cb58)?;
        Self::from_slice(&bytes)
    }
}

impl serde::Serialize for ShortId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for ShortId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        struct ShortIdVisitor;

        impl<'de> serde::de::Visitor<'de> for ShortIdVisitor {
            type Value = ShortId;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a CB58-encoded ShortId string or null")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> core::result::Result<ShortId, E> {
                v.parse::<ShortId>().map_err(E::custom)
            }

            fn visit_none<E: serde::de::Error>(self) -> core::result::Result<ShortId, E> {
                Ok(ShortId::default())
            }

            fn visit_unit<E: serde::de::Error>(self) -> core::result::Result<ShortId, E> {
                Ok(ShortId::default())
            }
        }

        deserializer.deserialize_any(ShortIdVisitor)
    }
}

impl core::fmt::Debug for ShortId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ShortId({})", self)
    }
}

impl From<[u8; SHORT_ID_LEN]> for ShortId {
    fn from(bytes: [u8; SHORT_ID_LEN]) -> ShortId {
        ShortId(bytes)
    }
}
