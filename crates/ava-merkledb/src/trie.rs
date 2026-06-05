// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! An in-memory path-based Merkle radix trie over a fixed key/value set.
//!
//! This is the shared trie used by both [`crate::hashing::merkle_root`] (root of
//! a fixed K/V set) and the DB-backed [`crate::view::View`] (which builds the
//! full merged set and then diffs the resulting node set against its parent).
//!
//! Byte-exact port of the relevant insert/hash logic of Go `x/merkledb`
//! (`view.go` + `node.go`). Insertion order does **not** affect the result —
//! children are kept in a [`BTreeMap`] keyed by branch-token byte so iteration,
//! hashing and serialization are always in ascending index order (no `HashMap`
//! on the serialization path, spec §6.1).

use std::collections::BTreeMap;

use bytes::Bytes;

use ava_types::id::Id;

use crate::hashing::{HASH_LENGTH, Hasher};
use crate::key::{BranchFactor, Key, longest_common_prefix};
use crate::maybe::Maybe;
use crate::node::{Child, DbNode, Node};

/// An owned trie node: its full key, optional value, and owned children keyed by
/// branch-token byte.
pub(crate) struct OwnedNode {
    /// This node's full key (bit-path from the root).
    pub key: Key,
    /// The node's value, if any.
    pub value: Maybe<Bytes>,
    /// Children keyed by branch-token byte (ascending).
    pub children: BTreeMap<u8, OwnedNode>,
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

/// A trie that supports insertion/deletion and root computation.
pub(crate) struct Trie {
    root: Option<OwnedNode>,
    token_size: usize,
    branch_factor: BranchFactor,
}

impl Trie {
    pub(crate) fn new(branch_factor: BranchFactor) -> Self {
        Trie {
            root: None,
            token_size: branch_factor.token_size(),
            branch_factor,
        }
    }

    /// The branch factor this trie was built with. Reserved for proof
    /// construction (M1.17).
    #[allow(dead_code)]
    pub(crate) fn branch_factor(&self) -> BranchFactor {
        self.branch_factor
    }

    /// Inserts or deletes `key`: `Maybe::Some` sets the value,
    /// `Maybe::Nothing` removes it. Byte-exact port of Go `view.insert` /
    /// `view.remove`.
    pub(crate) fn apply(&mut self, key: Key, value: Maybe<Bytes>) {
        match value {
            Maybe::Some(v) => self.insert(key, Maybe::Some(v)),
            Maybe::Nothing => self.remove(&key),
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

        // If the root's key isn't a prefix of [key], create a new branching root
        // over the common prefix (Go: closestNode == nil case).
        {
            let root_key = self
                .root
                .as_ref()
                .map_or_else(Key::empty, |r| r.key.clone());
            if !key.has_prefix(&root_key) {
                let common_prefix_length = longest_common_prefix(&root_key, &key, 0, token_size);
                let common_prefix = root_key.take(common_prefix_length);
                let Some(old_root) = self.root.take() else {
                    return;
                };
                let mut new_root = OwnedNode::new(common_prefix.clone());
                let branch_token = old_root.key.token(common_prefix.length(), token_size);
                new_root.children.insert(branch_token, old_root);
                self.root = Some(new_root);
            }
        }

        // Descend to the closest node along the path to [key].
        let path = self.visit_path_to_key(&key);
        let Some(closest) = self.node_at_path(&path) else {
            return;
        };

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
        let common_prefix_length = longest_common_prefix(
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

    /// Removes `key`. Mirrors the value-clearing + compaction of Go
    /// `view.remove`: clears the value at `key` if it exists, then prunes empty
    /// branches / collapses single-child valueless nodes.
    fn remove(&mut self, key: &Key) {
        let token_size = self.token_size;
        let path = self.visit_path_to_key(key);
        let Some(target) = self.node_at_path(&path) else {
            return;
        };
        if &target.key != key {
            // Key isn't present.
            return;
        }
        target.value = Maybe::Nothing;
        // Compact bottom-up along the visited path.
        self.compact_path(&path, token_size);
    }

    /// Collapses valueless single-child nodes and prunes empty leaves along
    /// `path` (root last). Mirrors the structural cleanup of Go `view.remove`.
    fn compact_path(&mut self, path: &[u8], token_size: usize) {
        // Process from the deepest node up to (but excluding) the root.
        for depth in (0..=path.len()).rev() {
            let prefix = &path[..depth];
            // The node's parent index within `path` is prefix[depth-1].
            if depth == 0 {
                // Root: if it has no value and no children, drop it; if it has no
                // value and exactly one child, the child can't be hoisted (root
                // keeps its key), so leave it. We only drop a fully-empty root.
                if let Some(root) = self.root.as_ref()
                    && root.value.is_nothing()
                    && root.children.is_empty()
                {
                    self.root = None;
                }
                continue;
            }
            let parent_path = &prefix[..depth - 1];
            let branch = prefix[depth - 1];
            let Some(parent) = self.node_at_path(parent_path) else {
                continue;
            };
            let Some(child) = parent.children.get(&branch) else {
                continue;
            };
            if child.value.has_value() {
                continue;
            }
            match child.children.len() {
                0 => {
                    // Empty valueless leaf: prune it.
                    parent.children.remove(&branch);
                }
                1 => {
                    // Valueless single-child node: collapse it into its child,
                    // which keeps its own (longer) full key.
                    let mut removed = match parent.children.remove(&branch) {
                        Some(c) => c,
                        None => continue,
                    };
                    let only_index = match removed.children.keys().next().copied() {
                        Some(i) => i,
                        None => continue,
                    };
                    if let Some(grandchild) = removed.children.remove(&only_index) {
                        let reattach_index =
                            grandchild.key.token(parent_key_len(parent), token_size);
                        parent.children.insert(reattach_index, grandchild);
                    }
                }
                _ => {}
            }
        }
    }

    /// Returns the sequence of branch-token indices from the root down to the
    /// closest node whose key is a prefix of `key` (and whose path matches).
    /// Mirrors Go `visitPathToKey`.
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
    /// the root, or `None` if the path can't be followed.
    fn node_at_path(&mut self, path: &[u8]) -> Option<&mut OwnedNode> {
        let mut current = self.root.as_mut()?;
        for index in path {
            current = current.children.get_mut(index)?;
        }
        Some(current)
    }

    /// Computes the merkle root. Mirrors Go `view.hashChangedNodes`.
    pub(crate) fn root_id<H: Hasher>(&self, hasher: &H) -> Id {
        match self.root.as_ref() {
            None => Id::EMPTY,
            Some(root) => hash_owned(hasher, root),
        }
    }

    /// Returns the value at `key`, if present. Reserved for proof construction
    /// (M1.17); the view's value lookups go through the merged map.
    #[allow(dead_code)]
    pub(crate) fn get(&self, key: &Key) -> Option<Bytes> {
        let mut current = self.root.as_ref()?;
        if !key.has_prefix(&current.key) {
            return None;
        }
        loop {
            if &current.key == key {
                return current.value.value().cloned();
            }
            let branch = key.token(current.key.length(), self.token_size);
            let next = current.children.get(&branch)?;
            let next_compressed = next.key.skip(current.key.length() + self.token_size);
            if !key.iterated_has_prefix(
                &next_compressed,
                current.key.length() + self.token_size,
                self.token_size,
            ) {
                return None;
            }
            current = next;
        }
    }

    /// Walks the trie, producing every node as a [`Node`] (with computed
    /// children IDs) keyed by full key. Used for storage diffing.
    pub(crate) fn nodes<H: Hasher>(&self, hasher: &H) -> BTreeMap<Key, Node> {
        let mut out = BTreeMap::new();
        if let Some(root) = self.root.as_ref() {
            collect_nodes(hasher, root, self.token_size, &mut out);
        }
        out
    }
}

/// The full key-length of `parent` (helper for borrow clarity in compaction).
fn parent_key_len(parent: &OwnedNode) -> usize {
    parent.key.length()
}

/// Recursively hashes an owned node bottom-up. Mirrors Go `hashChangedNode`.
fn hash_owned<H: Hasher>(hasher: &H, n: &OwnedNode) -> Id {
    let mut child_ids: BTreeMap<u8, Id> = BTreeMap::new();
    for (index, child) in &n.children {
        child_ids.insert(*index, hash_owned(hasher, child));
    }
    let value_digest = value_digest(hasher, &n.value);
    hasher.hash_node(&child_ids, &value_digest, &n.key)
}

/// Computes the value digest: the value if `len < 32`, else `HashValue`.
fn value_digest<H: Hasher>(hasher: &H, value: &Maybe<Bytes>) -> Maybe<Bytes> {
    match value {
        Maybe::Some(v) if v.len() >= HASH_LENGTH => {
            Maybe::Some(Bytes::copy_from_slice(hasher.hash_value(v).as_bytes()))
        }
        other => other.clone(),
    }
}

/// Recursively converts an [`OwnedNode`] (and descendants) into [`Node`]s with
/// computed child IDs, inserting into `out`. Returns the node's own ID.
fn collect_nodes<H: Hasher>(
    hasher: &H,
    n: &OwnedNode,
    token_size: usize,
    out: &mut BTreeMap<Key, Node>,
) -> Id {
    let mut children: BTreeMap<u8, Child> = BTreeMap::new();
    for (index, child) in &n.children {
        let child_id = collect_nodes(hasher, child, token_size, out);
        children.insert(
            *index,
            Child {
                compressed_key: child.key.skip(n.key.length() + token_size),
                id: child_id,
                has_value: child.value.has_value(),
            },
        );
    }
    let vd = value_digest(hasher, &n.value);
    let child_ids: BTreeMap<u8, Id> = children.iter().map(|(i, c)| (*i, c.id)).collect();
    let id = hasher.hash_node(&child_ids, &vd, &n.key);
    let node = Node {
        db_node: DbNode {
            value: n.value.clone(),
            children,
        },
        key: n.key.clone(),
        value_digest: vd,
    };
    out.insert(n.key.clone(), node);
    id
}
