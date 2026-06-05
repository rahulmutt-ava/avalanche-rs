// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Node model: [`DbNode`] (the on-disk node), [`Child`], and the in-memory
//! [`Node`] enrichment. Byte-exact port of Go `x/merkledb/node.go`.
//!
//! On-disk node key spaces (Go `db.go`, spec §10.8), documented here for the
//! later DB-wiring task:
//! - `0x00` `metadataPrefix` — `"cleanShutdown"` / `"root"`
//! - `0x01` `valueNodePrefix` — `Key.bytes()` -> node bytes (always durable)
//! - `0x02` `intermediateNodePrefix` — `Key.bytes()` -> node bytes (rebuildable)

use std::collections::BTreeMap;

use bytes::Bytes;

use ava_types::id::Id;

use crate::hashing::{HASH_LENGTH, Hasher};
use crate::key::Key;
use crate::maybe::Maybe;

/// On-disk node key-space prefixes (spec §10.8). Reserved for DB wiring.
pub mod prefix {
    /// `metadataPrefix` — clean-shutdown flag + persisted root key.
    pub const METADATA: u8 = 0x00;
    /// `valueNodePrefix` — value-node store (always durable).
    pub const VALUE_NODE: u8 = 0x01;
    /// `intermediateNodePrefix` — intermediate-node store (rebuildable).
    pub const INTERMEDIATE_NODE: u8 = 0x02;
}

/// A child entry of a [`DbNode`]. Mirrors Go `child`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    /// The portion of the child's full key below the parent + branch token.
    pub compressed_key: Key,
    /// The 32-byte id (hash) of the child node.
    pub id: Id,
    /// Whether the child node holds a value.
    pub has_value: bool,
}

/// The representation of a node stored in the database. Mirrors Go `dbNode`.
///
/// `children` is a [`BTreeMap`] keyed by the branch-token byte so iteration is
/// always in ascending index order — required by the byte-exact encoding and
/// hashing (no `HashMap` on the serialization path).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DbNode {
    /// The node's value, if any.
    pub value: Maybe<Bytes>,
    /// Child entries keyed by branch-token byte (ascending).
    pub children: BTreeMap<u8, Child>,
}

/// A node plus computation-friendly enrichment (its full key + value digest).
/// Mirrors Go `node`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// The on-disk node.
    pub db_node: DbNode,
    /// The node's full key (bit-path from the root).
    pub key: Key,
    /// The value digest: the value itself if `len < 32`, else `HashValue(value)`.
    pub value_digest: Maybe<Bytes>,
}

impl Node {
    /// Returns a new node with the given `key`, no value and no children.
    /// Mirrors Go `newNode`.
    #[must_use]
    pub fn new(key: Key) -> Node {
        Node {
            db_node: DbNode::default(),
            key,
            value_digest: Maybe::Nothing,
        }
    }

    /// `true` iff this node has a value. Mirrors Go `node.hasValue`.
    #[must_use]
    pub fn has_value(&self) -> bool {
        self.db_node.value.has_value()
    }

    /// Sets the node's value and recomputes the value digest.
    /// Mirrors Go `node.setValue`.
    pub fn set_value<H: Hasher>(&mut self, hasher: &H, value: Maybe<Bytes>) {
        self.db_node.value = value;
        self.set_value_digest(hasher);
    }

    /// Recomputes `value_digest` from `value`: the value itself if shorter than
    /// [`HASH_LENGTH`], else its hash. Mirrors Go `node.setValueDigest`.
    pub fn set_value_digest<H: Hasher>(&mut self, hasher: &H) {
        self.value_digest = match &self.db_node.value {
            Maybe::Some(v) if v.len() >= HASH_LENGTH => {
                Maybe::Some(Bytes::copy_from_slice(hasher.hash_value(v).as_bytes()))
            }
            other => other.clone(),
        };
    }
}
