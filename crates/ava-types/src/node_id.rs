// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 20-byte `NodeId` newtype with the `NodeID-` string prefix.
//!
//! Mirrors Go `ids.NodeID` — a distinct newtype (NOT an alias of `ShortID`) so
//! the `NodeID-` prefix and JSON form differ. The derived [`Ord`] is
//! lexicographic over the byte array (== Go `bytes.Compare`).
//!
//! `Display` outputs `NodeID-<cb58>`; `FromStr` requires the `NodeID-` prefix
//! (else returns [`Error::ShortNodeId`]). Serde serializes as the quoted Display
//! string; JSON `null` deserializes to `Default` (Go null no-op, spec §1.1).
//! TODO(M0.20): `From<[u8;20]>` is consumed by `ava-crypto::node_id_from_cert`.
//! Owning spec: `specs/03-core-primitives.md` §1.1, §3.6.

use crate::error::{Error, Result};

/// Length of a [`NodeId`] in bytes.
pub const NODE_ID_LEN: usize = 20;

/// The required string prefix for a [`NodeId`]. Mirrors Go `ids/node_id.go:17`.
pub const NODE_ID_PREFIX: &str = "NodeID-";

/// Node identifier. Mirrors `ids.NodeID`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct NodeId([u8; NODE_ID_LEN]);

impl NodeId {
    /// The all-zero node id.
    pub const EMPTY: NodeId = NodeId([0u8; NODE_ID_LEN]);

    /// Constructs a [`NodeId`] from a byte slice.
    ///
    /// # Errors
    /// Returns [`Error::InvalidHashLen`] if `bytes.len() != 20`.
    pub fn from_slice(bytes: &[u8]) -> Result<NodeId> {
        if bytes.len() != NODE_ID_LEN {
            return Err(Error::InvalidHashLen {
                expected: NODE_ID_LEN,
                actual: bytes.len(),
            });
        }
        let mut out = [0u8; NODE_ID_LEN];
        out.copy_from_slice(bytes);
        Ok(NodeId(out))
    }

    /// Returns a reference to the raw 20 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; NODE_ID_LEN] {
        &self.0
    }

    /// Consumes the node id, returning the raw 20 bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; NODE_ID_LEN] {
        self.0
    }

    /// Lowercase hex, no `0x` prefix.
    #[must_use]
    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl core::fmt::Display for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = ava_utils::cb58::cb58_encode(&self.0).map_err(|_| core::fmt::Error)?;
        write!(f, "{}{}", NODE_ID_PREFIX, s)
    }
}

impl core::str::FromStr for NodeId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let cb58 = s.strip_prefix(NODE_ID_PREFIX).ok_or_else(|| {
            Error::ShortNodeId(format!(
                "expected prefix '{}', got '{}'",
                NODE_ID_PREFIX, s
            ))
        })?;
        let bytes = ava_utils::cb58::cb58_decode(cb58).map_err(Error::Cb58)?;
        Self::from_slice(&bytes)
    }
}

impl serde::Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> core::result::Result<Self, D::Error> {
        struct NodeIdVisitor;

        impl<'de> serde::de::Visitor<'de> for NodeIdVisitor {
            type Value = NodeId;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a NodeID-prefixed CB58 string or null")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> core::result::Result<NodeId, E> {
                v.parse::<NodeId>().map_err(E::custom)
            }

            fn visit_none<E: serde::de::Error>(self) -> core::result::Result<NodeId, E> {
                Ok(NodeId::default())
            }

            fn visit_unit<E: serde::de::Error>(self) -> core::result::Result<NodeId, E> {
                Ok(NodeId::default())
            }
        }

        deserializer.deserialize_any(NodeIdVisitor)
    }
}

impl core::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "NodeId({})", self)
    }
}

impl From<[u8; NODE_ID_LEN]> for NodeId {
    fn from(bytes: [u8; NODE_ID_LEN]) -> NodeId {
        NodeId(bytes)
    }
}
