// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! 20-byte `NodeId` newtype with the `NodeID-` string prefix.
//!
//! Mirrors Go `ids.NodeID` — a distinct newtype (NOT an alias of `ShortID`) so
//! the `NodeID-` prefix and JSON form differ. The derived [`Ord`] is
//! lexicographic over the byte array (== Go `bytes.Compare`).
//!
//! TODO(M0.6): `Display`/`FromStr` requiring the `NodeID-` prefix; serde forms,
//! which depend on the CB58 codec being built in `ava-utils`.
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

impl core::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // TODO(M0.6): use CB58 Display (with the `NodeID-` prefix) once available.
        write!(f, "NodeId(0x{})", self.hex())
    }
}

impl From<[u8; NODE_ID_LEN]> for NodeId {
    fn from(bytes: [u8; NODE_ID_LEN]) -> NodeId {
        NodeId(bytes)
    }
}
