// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 32-byte `Id` newtype + ops (`prefix`/`append`/`xor`/`bit`).
//!
//! Mirrors Go `ids.ID` (`ids/id.go`). `prefix`/`append` use an inline
//! big-endian writer feeding `sha2::Sha256` directly so this crate does not
//! depend on `ava-codec`/`ava-crypto` (avoids the dependency cycle; see
//! `specs/03-core-primitives.md` §0 "Packer placement decision").
//!
//! `Display`/`FromStr` use bare CB58 (no prefix); serde serializes as the
//! quoted Display string; JSON `null` deserializes to `Default` (Go null no-op,
//! spec §1.1). CB58 lives in `ava-utils::cb58` to break the types→crypto cycle.
//! Owning spec: `specs/03-core-primitives.md` §1.1.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Length of an [`Id`] in bytes.
pub const ID_LEN: usize = 32;

/// 32-byte identifier (block IDs, tx IDs, chain IDs, subnet IDs, …).
///
/// Mirrors `ids.ID`. The derived [`Ord`] is lexicographic over the byte array,
/// which is exactly Go's `bytes.Compare`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Id([u8; ID_LEN]);

impl Id {
    /// The all-zero id. Mirrors `ids.Empty`.
    pub const EMPTY: Id = Id([0u8; ID_LEN]);

    /// Constructs an [`Id`] from a byte slice.
    ///
    /// # Errors
    /// Returns [`Error::InvalidHashLen`] if `bytes.len() != 32`
    /// (mirrors Go `hashing.ToHash256`).
    pub fn from_slice(bytes: &[u8]) -> Result<Id> {
        if bytes.len() != ID_LEN {
            return Err(Error::InvalidHashLen {
                expected: ID_LEN,
                actual: bytes.len(),
            });
        }
        let mut out = [0u8; ID_LEN];
        out.copy_from_slice(bytes);
        Ok(Id(out))
    }

    /// Returns a reference to the raw 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_LEN] {
        &self.0
    }

    /// Consumes the id, returning the raw 32 bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; ID_LEN] {
        self.0
    }

    /// `prefix` (consensus-relevant — `ids/id.go:97`).
    ///
    /// Concatenates `be_u64(prefixes[i])` for each prefix, then the 32 id bytes,
    /// and returns `sha256(...)` as a new [`Id`].
    #[must_use]
    pub fn prefix(&self, prefixes: &[u64]) -> Id {
        let mut hasher = Sha256::new();
        for p in prefixes {
            hasher.update(p.to_be_bytes());
        }
        hasher.update(self.0);
        Id(hasher.finalize().into())
    }

    /// ACP-77 validationID derivation (`ids/id.go:116`).
    ///
    /// Concatenates the 32 id bytes, then `be_u32(suffixes[i])` for each suffix,
    /// and returns `sha256(...)` as a new [`Id`].
    #[must_use]
    pub fn append(&self, suffixes: &[u32]) -> Id {
        let mut hasher = Sha256::new();
        hasher.update(self.0);
        for s in suffixes {
            hasher.update(s.to_be_bytes());
        }
        Id(hasher.finalize().into())
    }

    /// Byte-wise XOR of two ids.
    #[must_use]
    pub fn xor(&self, other: &Id) -> Id {
        let mut out = [0u8; ID_LEN];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.0[i] ^ other.0[i];
        }
        Id(out)
    }

    /// Returns bit `i` (0 or 1). `byte = i/8`, `bit = (b >> (i%8)) & 1`
    /// (`ids/id.go:140`).
    #[must_use]
    pub fn bit(&self, i: usize) -> u8 {
        (self.0[i / 8] >> (i % 8)) & 1
    }

    /// Lowercase hex, no `0x` prefix. Mirrors Go `id.Hex`.
    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl core::fmt::Display for Id {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // CB58 encode; cb58_encode only errors on oversized inputs (> i32::MAX - 4),
        // which a 32-byte array can never trigger, so the unwrap is infallible here.
        let s = ava_utils::cb58::cb58_encode(&self.0).map_err(|_| core::fmt::Error)?;
        f.write_str(&s)
    }
}

impl core::str::FromStr for Id {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = ava_utils::cb58::cb58_decode(s).map_err(Error::Cb58)?;
        Self::from_slice(&bytes)
    }
}

impl serde::Serialize for Id {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Id {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> core::result::Result<Self, D::Error> {
        struct IdVisitor;

        impl<'de> serde::de::Visitor<'de> for IdVisitor {
            type Value = Id;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a CB58-encoded Id string or null")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> core::result::Result<Id, E> {
                v.parse::<Id>().map_err(E::custom)
            }

            fn visit_none<E: serde::de::Error>(self) -> core::result::Result<Id, E> {
                Ok(Id::default())
            }

            fn visit_unit<E: serde::de::Error>(self) -> core::result::Result<Id, E> {
                Ok(Id::default())
            }
        }

        deserializer.deserialize_any(IdVisitor)
    }
}

impl core::fmt::Debug for Id {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Id({})", self)
    }
}

impl From<[u8; ID_LEN]> for Id {
    fn from(bytes: [u8; ID_LEN]) -> Id {
        Id(bytes)
    }
}
