// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Hashing: the [`Hasher`] trait + the protocol-fixed SHA-256 [`DefaultHasher`],
//! the canonical [`hash_node`] ordering, and a minimal in-memory trie builder
//! ([`merkle_root`]) for fixed K/V sets.
//!
//! Byte-exact port of Go `x/merkledb/hashing.go` (+ the relevant insert logic of
//! `view.go`). `hash_node` feeds the hasher in EXACTLY this order:
//! 1. `Uvarint(num_children)`.
//! 2. Per child in ascending byte-index order: `Uvarint(index)` then the
//!    child's 32-byte `id`.
//! 3. Value digest present ⇒ `0x01`, `Uvarint(len(digest))`, digest bytes; else
//!    `0x00`.
//! 4. `Uvarint(key.length)` (bits) then `key.bytes()`.
//!
//! `HashValue(v) = SHA-256(v)`, `HashLength = 32`. The root ID is `hash_node`
//! of the root; an EMPTY trie hashes to [`ava_types::id::Id::EMPTY`].
//!
//! The trie builder is intentionally minimal — just enough to compute roots for
//! a fixed K/V set. The full DB-backed `View`/`TrieView` is a later M1 task.

use std::collections::BTreeMap;

use bytes::Bytes;
use sha2::{Digest, Sha256};

use ava_types::id::Id;

use crate::key::{BranchFactor, Key};
use crate::maybe::Maybe;

/// The hash length in bytes. Mirrors Go `HashLength`.
pub const HASH_LENGTH: usize = 32;

/// A merkledb hasher. The protocol default is SHA-256 ([`DefaultHasher`]); the
/// trait exists only so the hash function is swappable in tests.
/// Mirrors Go `Hasher`.
pub trait Hasher {
    /// Returns the canonical hash of a node from its components.
    /// Mirrors Go `HashNode`.
    fn hash_node(&self, children: &BTreeMap<u8, Id>, value_digest: &Maybe<Bytes>, key: &Key) -> Id;

    /// Returns the canonical hash of `value`. Mirrors Go `HashValue`.
    fn hash_value(&self, value: &[u8]) -> Id;
}

/// The protocol-fixed SHA-256 hasher. Mirrors Go `sha256Hasher` /
/// `DefaultHasher`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultHasher;

/// Appends `v` to `out` as an unsigned LEB128 varint (Go `binary.AppendUvarint`).
fn append_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

impl Hasher for DefaultHasher {
    fn hash_node(&self, children: &BTreeMap<u8, Id>, value_digest: &Maybe<Bytes>, key: &Key) -> Id {
        let mut sha = Sha256::new();
        let mut buf: Vec<u8> = Vec::with_capacity(HASH_LENGTH);

        // 1. number of children.
        buf.clear();
        append_uvarint(&mut buf, children.len() as u64);
        sha.update(&buf);

        // 2. each child in ascending byte-index order: index then 32-byte id.
        //    BTreeMap iterates ascending by key, matching Go's `slices.Sort`.
        for (index, id) in children {
            buf.clear();
            append_uvarint(&mut buf, u64::from(*index));
            sha.update(&buf);
            sha.update(id.as_bytes());
        }

        // 3. the value digest.
        match value_digest {
            Maybe::Some(digest) => {
                sha.update([0x01]);
                buf.clear();
                append_uvarint(&mut buf, digest.len() as u64);
                sha.update(&buf);
                sha.update(digest);
            }
            Maybe::Nothing => {
                sha.update([0x00]);
            }
        }

        // 4. the key bit-length then its packed bytes.
        buf.clear();
        append_uvarint(&mut buf, key.length() as u64);
        sha.update(&buf);
        sha.update(key.bytes());

        Id::from(<[u8; HASH_LENGTH]>::from(sha.finalize()))
    }

    fn hash_value(&self, value: &[u8]) -> Id {
        let mut sha = Sha256::new();
        sha.update(value);
        Id::from(<[u8; HASH_LENGTH]>::from(sha.finalize()))
    }
}

// ---------------------------------------------------------------------------
// Minimal in-memory trie builder
// ---------------------------------------------------------------------------

/// An owned trie node used only to compute a root for a fixed K/V set.
///
/// Children are owned directly, keyed by their branch-token byte. A child's
/// `compressed_key` relative to its parent is derived on demand as
/// `child.key.skip(parent.key.length + token_size)`.
struct OwnedNode {
    /// This node's full key (bit-path from the root).
    key: Key,
    /// The node's value, if any.
    value: Maybe<Bytes>,
    /// Children keyed by branch-token byte (ascending).
    children: BTreeMap<u8, OwnedNode>,
}

impl OwnedNode {
    fn new(key: Key) -> OwnedNode {
        OwnedNode {
            key,
            value: Maybe::Nothing,
            children: BTreeMap::new(),
        }
    }
}

/// A minimal trie that supports insertion + root computation. Mirrors the
/// relevant subset of Go `view`.
struct TrieBuilder<'a, H: Hasher> {
    root: Option<OwnedNode>,
    token_size: usize,
    hasher: &'a H,
}

impl<'a, H: Hasher> TrieBuilder<'a, H> {
    fn new(branch_factor: BranchFactor, hasher: &'a H) -> Self {
        TrieBuilder {
            root: None,
            token_size: branch_factor.token_size(),
            hasher,
        }
    }

    /// Inserts `key`/`value`. Byte-exact port of Go `view.insert`.
    fn insert(&mut self, key: Key, value: Maybe<Bytes>) {
        let token_size = self.token_size;

        // Empty trie: create a new root node holding the value.
        if self.root.is_none() {
            let mut root = OwnedNode::new(key);
            root.value = value;
            self.root = Some(root);
            return;
        }

        // If the root's key isn't a prefix of [key], create a new branching
        // root over the common prefix (Go: closestNode == nil case).
        {
            let root_key = self
                .root
                .as_ref()
                .map(|r| r.key.clone())
                .unwrap_or_else(Key::empty);
            if !key.has_prefix(&root_key) {
                let common_prefix_length =
                    crate::key::longest_common_prefix(&root_key, &key, 0, token_size);
                let common_prefix = root_key.take(common_prefix_length);
                let old_root = self.root.take().expect("root present");
                let mut new_root = OwnedNode::new(common_prefix.clone());
                let branch_token = old_root.key.token(common_prefix.length(), token_size);
                new_root.children.insert(branch_token, old_root);
                self.root = Some(new_root);
            }
        }

        // Descend to the closest node along the path to [key].
        let path = self.visit_path_to_key(&key);

        // SAFETY of indexing: [path] holds the indices from the root down to the
        // closest node; following them always lands on existing children.
        let closest = self.node_at_path(&path);

        // Exact match: update value.
        if closest.key == key {
            closest.value = value;
            return;
        }

        // Determine the unmatched portion. [key] has prefix closest.key.
        let branch_index = key.token(closest.key.length(), token_size);
        let Some(existing) = closest.children.remove(&branch_index) else {
            // No existing node along [key]; create a new child leaf.
            let mut new_node = OwnedNode::new(key);
            new_node.value = value;
            closest.children.insert(branch_index, new_node);
            return;
        };
        let existing_compressed = existing.key.skip(closest.key.length() + token_size);
        let common_prefix_length = crate::key::longest_common_prefix(
            &existing_compressed,
            &key,
            closest.key.length() + token_size,
            token_size,
        );

        // The branch node sits at the common prefix.
        let branch_key = key.take(closest.key.length() + token_size + common_prefix_length);
        let mut branch_node = OwnedNode::new(branch_key.clone());

        if key.length() == branch_node.key.length() {
            // The branch node IS the key being inserted.
            branch_node.value = value;
        } else {
            // The key is a child of the branch node.
            let mut leaf = OwnedNode::new(key.clone());
            leaf.value = value;
            let leaf_index = key.token(branch_node.key.length(), token_size);
            branch_node.children.insert(leaf_index, leaf);
        }

        // Re-attach the existing child onto the branch node.
        let existing_index = existing_compressed.token(common_prefix_length, token_size);
        branch_node.children.insert(existing_index, existing);

        closest.children.insert(branch_index, branch_node);
    }

    /// Returns the sequence of branch-token indices from the root down to the
    /// closest node whose key is a prefix of [key] (and whose path matches).
    /// Mirrors Go `visitPathToKey` (returns the deepest visited node's path).
    fn visit_path_to_key(&self, key: &Key) -> Vec<u8> {
        let mut path = Vec::new();
        let Some(root) = self.root.as_ref() else {
            return path;
        };
        if !key.has_prefix(&root.key) {
            return path;
        }
        let token_size = self.token_size;
        let mut current = root;
        while current.key.length() < key.length() {
            let branch = key.token(current.key.length(), token_size);
            let Some(next) = current.children.get(&branch) else {
                break;
            };
            let next_compressed = next.key.skip(current.key.length() + token_size);
            if !key.iterated_has_prefix(
                &next_compressed,
                current.key.length() + token_size,
                token_size,
            ) {
                break;
            }
            path.push(branch);
            current = next;
        }
        path
    }

    /// Returns a mutable reference to the node reached by following `path` from
    /// the root.
    fn node_at_path(&mut self, path: &[u8]) -> &mut OwnedNode {
        let mut current = self.root.as_mut().expect("root present");
        for index in path {
            current = current.children.get_mut(index).expect("path index present");
        }
        current
    }

    /// Computes the merkle root. Mirrors Go `view.hashChangedNodes`.
    fn root_id(&self) -> Id {
        match self.root.as_ref() {
            None => Id::EMPTY,
            Some(root) => self.hash_owned(root),
        }
    }

    /// Recursively hashes an owned node bottom-up. Mirrors Go `hashChangedNode`.
    fn hash_owned(&self, n: &OwnedNode) -> Id {
        let mut child_ids: BTreeMap<u8, Id> = BTreeMap::new();
        for (index, child) in &n.children {
            child_ids.insert(*index, self.hash_owned(child));
        }
        let value_digest = self.value_digest(&n.value);
        self.hasher.hash_node(&child_ids, &value_digest, &n.key)
    }

    /// Computes the value digest: the value if `len < 32`, else `HashValue`.
    /// Mirrors Go `node.setValueDigest`.
    fn value_digest(&self, value: &Maybe<Bytes>) -> Maybe<Bytes> {
        match value {
            Maybe::Some(v) if v.len() >= HASH_LENGTH => {
                Maybe::Some(Bytes::copy_from_slice(self.hasher.hash_value(v).as_bytes()))
            }
            other => other.clone(),
        }
    }
}

/// Computes the merkle root ID of the trie holding exactly `kvs` under the given
/// `branch_factor`, using `hasher`. An empty `kvs` yields
/// [`ava_types::id::Id::EMPTY`]. Insertion order does not affect the result.
pub fn merkle_root<H: Hasher>(
    branch_factor: BranchFactor,
    hasher: &H,
    kvs: &[(&[u8], &[u8])],
) -> Id {
    let mut builder = TrieBuilder::new(branch_factor, hasher);
    for (k, v) in kvs {
        builder.insert(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
    }
    builder.root_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_root_is_id_empty() {
        let h = DefaultHasher;
        assert_eq!(merkle_root(BranchFactor::TwoFiftySix, &h, &[]), Id::EMPTY);
    }

    #[test]
    fn order_independent_root() {
        let h = DefaultHasher;
        let a = merkle_root(
            BranchFactor::TwoFiftySix,
            &h,
            &[(b"dog", b"woof"), (b"cat", b"meow"), (b"do", b"verb")],
        );
        let b = merkle_root(
            BranchFactor::TwoFiftySix,
            &h,
            &[(b"do", b"verb"), (b"dog", b"woof"), (b"cat", b"meow")],
        );
        assert_eq!(a, b);
    }
}
