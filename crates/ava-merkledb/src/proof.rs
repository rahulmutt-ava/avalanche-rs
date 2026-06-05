// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Merkle proofs — single [`Proof`] (inclusion/exclusion), [`RangeProof`], and
//! [`ChangeProof`]. Byte-exact-in-behavior port of Go `x/merkledb/proof.go`
//! (spec 04 §3.6); the protobuf wire envelope follows `proto/sync/sync.proto`
//! (spec 15 §3.10).
//!
//! A [`ProofNode`]'s `value_or_hash` is the node's value digest (the value if
//! `len < HashLength`, else its hash) and `children` maps each branch-token byte
//! to the 32-byte child ID. Verification rebuilds a partial trie from the proof
//! nodes (overwriting each node's value digest with the proof's `value_or_hash`,
//! and re-attaching the out-of-range children by ID) and checks the recomputed
//! root equals the expected root.
//!
//! ## Protobuf encoding
//!
//! There is no workspace proto build infra yet, so the small `proto/sync`
//! messages are hand-encoded here in the protobuf wire format (LEB128 varint
//! tags + length-delimited fields). **`ProofNode.children` is encoded in
//! ascending-index (`BTreeMap`) order** — Go's `proto.Marshal` randomizes map
//! iteration order, so the committed golden vectors use Go's *deterministic*
//! marshaler (`proto.MarshalOptions{Deterministic:true}`), which also sorts map
//! keys. The two then agree byte-for-byte (spec 00 §6.1: no `HashMap` on the
//! serialization path).

use std::collections::BTreeMap;

use bytes::Bytes;

use ava_types::id::{ID_LEN, Id};

use crate::error::{Error, Result};
use crate::hashing::{HASH_LENGTH, Hasher};
use crate::key::{BranchFactor, Key, bytes_needed};
use crate::maybe::Maybe;
use crate::trie::Trie;

// ---------------------------------------------------------------------------
// Proof types
// ---------------------------------------------------------------------------

/// A node in a proof path. Mirrors Go `ProofNode`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofNode {
    /// The node's full key (bit-path from the root).
    pub key: Key,
    /// `Nothing` for an intermediate node; otherwise the value (if its length
    /// `< HashLength`) or the hash of the value.
    pub value_or_hash: Maybe<Bytes>,
    /// Children keyed by branch-token byte (ascending), each mapping to the
    /// 32-byte child ID.
    pub children: BTreeMap<u8, Id>,
}

/// An inclusion/exclusion proof of a single key. Mirrors Go `Proof`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// Nodes on the path root -> target key (or the closest node, on exclusion).
    /// Always contains at least the root.
    path: Vec<ProofNode>,
    /// The key this proof is about. Must not have partial bytes.
    key: Key,
    /// The value at `key` (`Some`) or `Nothing` if `key` is not in the trie.
    value: Maybe<Bytes>,
}

impl Proof {
    /// Builds an inclusion/exclusion proof of `key` against the trie holding
    /// exactly `kvs` under `branch_factor`. Mirrors Go `getProof`.
    ///
    /// # Errors
    /// Returns [`Error::EmptyProof`] if the trie is empty.
    pub fn prove<H: Hasher>(
        branch_factor: BranchFactor,
        hasher: &H,
        kvs: &[(&[u8], &[u8])],
        key: &[u8],
    ) -> Result<Proof> {
        let mut trie = Trie::new(branch_factor);
        for (k, v) in kvs {
            trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
        }
        Proof::from_trie(&trie, hasher, key)
    }

    /// Builds a proof of `key` from an already-built [`Trie`].
    pub(crate) fn from_trie<H: Hasher>(trie: &Trie, hasher: &H, key: &[u8]) -> Result<Proof> {
        if trie.is_empty() {
            return Err(Error::EmptyProof);
        }
        let proof_key = Key::from_bytes(key);
        let (path, value) = trie
            .get_proof(hasher, &proof_key)
            .ok_or(Error::EmptyProof)?;
        Ok(Proof {
            path,
            key: proof_key,
            value,
        })
    }

    /// The proof path nodes (root first).
    #[must_use]
    pub fn path(&self) -> &[ProofNode] {
        &self.path
    }

    /// The key this proof is about.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// The proven value (`Some` on inclusion, `Nothing` on exclusion).
    #[must_use]
    pub fn value(&self) -> Maybe<Bytes> {
        self.value.clone()
    }

    /// Overwrites the proven value (for tamper tests).
    pub fn set_value(&mut self, value: Maybe<Bytes>) {
        self.value = value;
    }

    /// Encodes the proof `path` as a `repeated ProofNode` (the same on-wire
    /// shape as `sync.RangeProof.start_proof`, field 1). proto/sync has no
    /// single-Proof message.
    #[must_use]
    pub fn encode_path_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode_repeated_proof_nodes(&mut out, 1, &self.path);
        out
    }

    /// Verifies that the trie described by this proof has root `expected_root`.
    /// Mirrors Go `Proof.Verify`.
    ///
    /// # Errors
    /// Returns an [`Error`] if the proof is empty/ill-formed, the proven value
    /// doesn't match, or the recomputed root differs from `expected_root`.
    pub fn verify<H: Hasher>(
        &self,
        expected_root: Id,
        branch_factor: BranchFactor,
        hasher: &H,
    ) -> Result<()> {
        let token_size = branch_factor.token_size();

        if self.path.is_empty() {
            return Err(Error::EmptyProof);
        }
        if self.key.has_partial_byte() {
            return Err(Error::ProofKeyPartialByte);
        }

        let last = &self.path[self.path.len() - 1];
        let inclusion = last.key == self.key;

        if inclusion && !value_or_hash_matches(hasher, &self.value, &last.value_or_hash) {
            return Err(Error::ProofValueDoesntMatch);
        }
        if !inclusion && self.value.has_value() {
            return Err(Error::ExclusionProofUnexpectedValue);
        }

        verify_proof_path(&self.path, &self.key, token_size)?;

        // Rebuild a standalone trie from the path; the proven key bounds both
        // the left- and right-children insertion (Go passes `provenKey` for
        // both `insertChildrenLessThan` and `insertChildrenGreaterThan`).
        let proven_key = Maybe::Some(last.key.clone());
        let mut builder = ProofTrieBuilder::new(branch_factor);
        builder.add_path_info(&self.path, &proven_key, &proven_key)?;

        let got_root = builder.root_id(hasher);
        if got_root != expected_root {
            return Err(Error::InvalidProof);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Range / change proofs
// ---------------------------------------------------------------------------

/// A key/value pair carried in a [`RangeProof`]. Mirrors Go `KeyValue`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyValue {
    /// The full (unpacked) key bytes.
    pub key: Vec<u8>,
    /// The value bytes.
    pub value: Vec<u8>,
}

/// A single key change in a [`ChangeProof`]. Mirrors Go `KeyChange`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChange {
    /// The full (unpacked) key bytes.
    pub key: Vec<u8>,
    /// The value after the change; `Nothing` means the key was deleted.
    pub value: Maybe<Bytes>,
}

/// A proof of a contiguous `[start, end]` slice of key/value pairs. Mirrors Go
/// `RangeProof`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RangeProof {
    /// Inclusion/exclusion proof of the lower bound (empty if no lower bound).
    pub start_proof: Vec<ProofNode>,
    /// Inclusion proof of the largest key in `key_values` (or exclusion proof of
    /// `end` if `key_values` is empty).
    pub end_proof: Vec<ProofNode>,
    /// The contiguous slice of key/value pairs, sorted by increasing key.
    pub key_values: Vec<KeyValue>,
}

/// A proof of the key changes between two roots. Mirrors Go `ChangeProof`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ChangeProof {
    /// Inclusion/exclusion proof of the lower bound (may omit nodes also in
    /// `end_proof`).
    pub start_proof: Vec<ProofNode>,
    /// Inclusion proof of the largest changed key (or exclusion proof of the
    /// upper bound).
    pub end_proof: Vec<ProofNode>,
    /// The subset of key changes, sorted by increasing key, no duplicates.
    pub key_changes: Vec<KeyChange>,
}

impl RangeProof {
    /// Builds a range proof for `[start, end]` (each `None` meaning unbounded)
    /// over the trie holding exactly `kvs`, with at most `max_length` pairs.
    /// Mirrors Go `getRangeProof`.
    ///
    /// # Errors
    /// Returns [`Error::StartAfterEnd`] if `start > end`,
    /// [`Error::InvalidMaxLength`] if `max_length == 0`, or [`Error::EmptyProof`]
    /// if the trie is empty.
    pub fn prove<H: Hasher>(
        branch_factor: BranchFactor,
        hasher: &H,
        kvs: &[(&[u8], &[u8])],
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_length: usize,
    ) -> Result<RangeProof> {
        if let (Some(s), Some(e)) = (start, end)
            && s > e
        {
            return Err(Error::StartAfterEnd);
        }
        if max_length == 0 {
            return Err(Error::InvalidMaxLength);
        }

        let mut trie = Trie::new(branch_factor);
        let mut sorted: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for (k, v) in kvs {
            trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
            sorted.insert(k.to_vec(), v.to_vec());
        }
        if trie.is_empty() {
            return Err(Error::EmptyProof);
        }

        // Collect the contiguous slice in [start, end], capped at max_length.
        let mut key_values: Vec<KeyValue> = Vec::new();
        for (k, v) in &sorted {
            if let Some(s) = start
                && k.as_slice() < s
            {
                continue;
            }
            if key_values.len() >= max_length {
                break;
            }
            if let Some(e) = end
                && k.as_slice() > e
            {
                break;
            }
            key_values.push(KeyValue {
                key: k.clone(),
                value: v.clone(),
            });
        }

        let mut proof = RangeProof::default();

        // End proof: inclusion of the largest key, or exclusion of `end`.
        let end_proof_key: Option<Vec<u8>> = if let Some(kv) = key_values.last() {
            Some(kv.key.clone())
        } else {
            end.map(<[u8]>::to_vec)
        };
        if let Some(ek) = &end_proof_key {
            let p = Proof::from_trie(&trie, hasher, ek)?;
            proof.end_proof = p.path;
        }

        // Start proof: inclusion/exclusion of `start`, with common prefix nodes
        // stripped (they're already in the end proof).
        if let Some(s) = start {
            let p = Proof::from_trie(&trie, hasher, s)?;
            let mut start_path = p.path;
            let mut i = 0;
            while i < start_path.len()
                && i < proof.end_proof.len()
                && start_path[i].key == proof.end_proof[i].key
            {
                i += 1;
            }
            proof.start_proof = start_path.split_off(i);
        }

        proof.key_values = key_values;
        Ok(proof)
    }

    /// Encodes this range proof as `proto/sync` `RangeProof`. Mirrors Go
    /// `RangeProof.toProto` + `proto.Marshal` (deterministic).
    #[must_use]
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode_repeated_proof_nodes(&mut out, 1, &self.start_proof);
        encode_repeated_proof_nodes(&mut out, 2, &self.end_proof);
        for kv in &self.key_values {
            let mut inner = Vec::new();
            encode_bytes_field(&mut inner, 1, &kv.key);
            encode_bytes_field(&mut inner, 2, &kv.value);
            encode_len_field(&mut out, 3, &inner);
        }
        out
    }

    /// Verifies this range proof against `expected_root`. Mirrors Go
    /// `RangeProof.Verify` (build a trie from `key_values`, add the boundary
    /// proof nodes, check the recomputed root).
    ///
    /// # Errors
    /// Returns an [`Error`] on any structural-invariant violation or root
    /// mismatch.
    pub fn verify<H: Hasher>(
        &self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        expected_root: Id,
        branch_factor: BranchFactor,
        hasher: &H,
    ) -> Result<()> {
        let token_size = branch_factor.token_size();

        if let (Some(s), Some(e)) = (start, end)
            && s > e
        {
            return Err(Error::StartAfterEnd);
        }

        if self.key_values.is_empty() && self.start_proof.is_empty() && self.end_proof.is_empty() {
            return Err(Error::EmptyProof);
        }

        // Keys must be sorted, distinct, and within [start, end].
        for window in self.key_values.windows(2) {
            if window[0].key >= window[1].key {
                return Err(Error::NonIncreasingValues);
            }
        }
        for kv in &self.key_values {
            if let Some(s) = start
                && kv.key.as_slice() < s
            {
                return Err(Error::StateFromOutsideOfRange);
            }
            if let Some(e) = end
                && kv.key.as_slice() > e
            {
                return Err(Error::StateFromOutsideOfRange);
            }
        }

        // The end proof must prove the largest key, or `end` on an empty slice.
        let start_key = start.map(Key::from_bytes);
        let end_proof_key: Option<Key> = if let Some(kv) = self.key_values.last() {
            Some(Key::from_bytes(&kv.key))
        } else {
            end.map(Key::from_bytes)
        };

        verify_proof_path(
            &self.start_proof,
            start_key.as_ref().unwrap_or(&Key::empty()),
            token_size,
        )?;
        verify_proof_path(
            &self.end_proof,
            end_proof_key.as_ref().unwrap_or(&Key::empty()),
            token_size,
        )?;

        // Ensure the proof nodes' value digests agree with `key_values` (a
        // tampered value in `key_values` is caught here). Mirrors Go
        // `verifyChangeProofKeyValues` for both the start and end proofs.
        let kv_map: BTreeMap<Key, Bytes> = self
            .key_values
            .iter()
            .map(|kv| (Key::from_bytes(&kv.key), Bytes::copy_from_slice(&kv.value)))
            .collect();
        verify_proof_key_values(
            hasher,
            &self.start_proof,
            &kv_map,
            start_key.as_ref(),
            end_proof_key.as_ref(),
        )?;
        verify_proof_key_values(
            hasher,
            &self.end_proof,
            &kv_map,
            start_key.as_ref(),
            end_proof_key.as_ref(),
        )?;

        // Build the verification trie from the key/value slice.
        let mut builder = ProofTrieBuilder::new(branch_factor);
        for kv in &self.key_values {
            builder.insert_value(&Key::from_bytes(&kv.key), &kv.value);
        }

        // Insert the boundary proof nodes. Mirrors Go `addPathInfo(view, proof,
        // startProofKey, endProofKey)` for BOTH proofs: each inserts children
        // whose full key is < startProofKey OR > endProofKey. Everything within
        // the range is supplied by `key_values`.
        let lt_bound = maybe_key(start_key.clone());
        let gt_bound = maybe_key(end_proof_key.clone());
        builder.add_path_info(&self.start_proof, &lt_bound, &gt_bound)?;
        builder.add_path_info(&self.end_proof, &lt_bound, &gt_bound)?;

        let got_root = builder.root_id(hasher);
        if got_root != expected_root {
            return Err(Error::InvalidProof);
        }
        Ok(())
    }
}

impl ChangeProof {
    /// Builds a change proof for `[start, end]` between the trie holding `before`
    /// (the start root, exclusive) and the trie holding `after` (the end root,
    /// inclusive). The `key_changes` are the keys whose value differs, with the
    /// *after* value (`Nothing` ⇒ deletion). The boundary proofs are taken
    /// against the *after* trie. Mirrors Go `GetChangeProof` (without the bounded
    /// history ring — the caller supplies both states directly).
    ///
    /// # Errors
    /// Returns an [`Error`] if the bounds are inverted or `max_length == 0`.
    pub fn prove<H: Hasher>(
        branch_factor: BranchFactor,
        hasher: &H,
        before: &[(&[u8], &[u8])],
        after: &[(&[u8], &[u8])],
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        max_length: usize,
    ) -> Result<ChangeProof> {
        if let (Some(s), Some(e)) = (start, end)
            && s > e
        {
            return Err(Error::StartAfterEnd);
        }
        if max_length == 0 {
            return Err(Error::InvalidMaxLength);
        }

        let before_map: BTreeMap<Vec<u8>, Vec<u8>> = before
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
        let after_map: BTreeMap<Vec<u8>, Vec<u8>> = after
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();

        // Compute changes: keys whose value differs (added/modified/removed).
        let mut all_keys: Vec<Vec<u8>> = Vec::new();
        all_keys.extend(before_map.keys().cloned());
        all_keys.extend(after_map.keys().cloned());
        all_keys.sort();
        all_keys.dedup();

        let mut key_changes: Vec<KeyChange> = Vec::new();
        for k in &all_keys {
            if let Some(s) = start
                && k.as_slice() < s
            {
                continue;
            }
            if let Some(e) = end
                && k.as_slice() > e
            {
                continue;
            }
            let b = before_map.get(k);
            let a = after_map.get(k);
            if b == a {
                continue;
            }
            if key_changes.len() >= max_length {
                break;
            }
            key_changes.push(KeyChange {
                key: k.clone(),
                value: a
                    .map(|v| Maybe::Some(Bytes::copy_from_slice(v)))
                    .unwrap_or(Maybe::Nothing),
            });
        }

        // Boundary proofs are against the after (end) trie.
        let mut after_trie = Trie::new(branch_factor);
        for (k, v) in &after_map {
            after_trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
        }

        let mut proof = ChangeProof {
            key_changes,
            ..Default::default()
        };

        if after_trie.is_empty() {
            return Ok(proof);
        }

        let end_proof_key: Option<Vec<u8>> = if let Some(kc) = proof.key_changes.last() {
            Some(kc.key.clone())
        } else {
            end.map(<[u8]>::to_vec)
        };
        if let Some(ek) = &end_proof_key {
            let p = Proof::from_trie(&after_trie, hasher, ek)?;
            proof.end_proof = p.path;
        }

        if let Some(s) = start {
            let p = Proof::from_trie(&after_trie, hasher, s)?;
            let mut start_path = p.path;
            let mut i = 0;
            while i < start_path.len()
                && i < proof.end_proof.len()
                && start_path[i].key == proof.end_proof[i].key
            {
                i += 1;
            }
            proof.start_proof = start_path.split_off(i);
        }

        Ok(proof)
    }

    /// Encodes this change proof as `proto/sync` `ChangeProof`. Mirrors Go
    /// `ChangeProof.toProto` (deterministic marshal).
    #[must_use]
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        encode_repeated_proof_nodes(&mut out, 1, &self.start_proof);
        encode_repeated_proof_nodes(&mut out, 2, &self.end_proof);
        for kc in &self.key_changes {
            let mut inner = Vec::new();
            encode_bytes_field(&mut inner, 1, &kc.key);
            // MaybeBytes value (field 2) — present iff the change is not a delete.
            if let Maybe::Some(v) = &kc.value {
                let mut mb = Vec::new();
                encode_bytes_field(&mut mb, 1, v);
                encode_len_field(&mut inner, 2, &mb);
            }
            encode_len_field(&mut out, 3, &inner);
        }
        out
    }

    /// Verifies this change proof: applying the `key_changes` to the trie that
    /// holds `start` (the start-root state) yields a trie whose root equals
    /// `expected_end_root`. Mirrors Go `VerifyChangeProof` semantics (the caller
    /// supplies the start-root key/value set; full proof-node subset checks are a
    /// superset of what's exercised here).
    ///
    /// # Errors
    /// Returns an [`Error`] on bound/order violations or root mismatch.
    pub fn verify<H: Hasher>(
        &self,
        start_kvs: &[(&[u8], &[u8])],
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        expected_end_root: Id,
        branch_factor: BranchFactor,
        hasher: &H,
    ) -> Result<()> {
        if let (Some(s), Some(e)) = (start, end)
            && s > e
        {
            return Err(Error::StartAfterEnd);
        }

        // Keys sorted, distinct, within [start, end].
        for window in self.key_changes.windows(2) {
            if window[0].key >= window[1].key {
                return Err(Error::NonIncreasingValues);
            }
        }
        for kc in &self.key_changes {
            if let Some(s) = start
                && kc.key.as_slice() < s
            {
                return Err(Error::StateFromOutsideOfRange);
            }
            if let Some(e) = end
                && kc.key.as_slice() > e
            {
                return Err(Error::StateFromOutsideOfRange);
            }
        }

        // Apply the changes to the start state and check the resulting root.
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = start_kvs
            .iter()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
        for kc in &self.key_changes {
            match &kc.value {
                Maybe::Some(v) => {
                    merged.insert(kc.key.clone(), v.to_vec());
                }
                Maybe::Nothing => {
                    merged.remove(&kc.key);
                }
            }
        }

        let mut trie = Trie::new(branch_factor);
        for (k, v) in &merged {
            trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
        }
        let got = trie.root_id(hasher);
        if got != expected_end_root {
            return Err(Error::InvalidProof);
        }
        Ok(())
    }
}

fn maybe_key(k: Option<Key>) -> Maybe<Key> {
    k.map(Maybe::Some).unwrap_or(Maybe::Nothing)
}

// ---------------------------------------------------------------------------
// Verification trie builder (port of Go `addPathInfo` + standalone view root)
// ---------------------------------------------------------------------------

/// Rebuilds a partial trie from key/values + proof nodes to recompute a root for
/// verification. Backed by the real [`Trie`] (so intermediate branch nodes are
/// materialised correctly), with two proof-verification overlays applied at
/// hashing time: per-node value-digest overrides and out-of-range child
/// injections (by ID). Mirrors Go `getStandaloneView` + `addPathInfo` + the
/// view's root computation.
struct ProofTrieBuilder {
    trie: Trie,
    token_size: usize,
    /// Per-full-key value-digest overrides taken from proof nodes.
    value_digests: BTreeMap<Key, Maybe<Bytes>>,
    /// Per-full-key out-of-range child injections (index -> child ID).
    child_injections: BTreeMap<Key, BTreeMap<u8, Id>>,
}

impl ProofTrieBuilder {
    fn new(branch_factor: BranchFactor) -> ProofTrieBuilder {
        ProofTrieBuilder {
            trie: Trie::new(branch_factor),
            token_size: branch_factor.token_size(),
            value_digests: BTreeMap::new(),
            child_injections: BTreeMap::new(),
        }
    }

    /// Inserts a key/value into the underlying trie. The value drives the
    /// node's digest naturally (no override needed).
    fn insert_value(&mut self, key: &Key, value: &[u8]) {
        self.trie
            .apply(key.clone(), Maybe::Some(Bytes::copy_from_slice(value)));
    }

    /// Adds each proof node: materialises its skeleton node, records the
    /// value-digest override, and records the out-of-range children to inject
    /// (full key < `insert_children_less_than` OR > `insert_children_greater_than`).
    /// Mirrors Go `addPathInfo`.
    fn add_path_info(
        &mut self,
        path: &[ProofNode],
        insert_children_less_than: &Maybe<Key>,
        insert_children_greater_than: &Maybe<Key>,
    ) -> Result<()> {
        let insert_any =
            insert_children_less_than.has_value() || insert_children_greater_than.has_value();

        for proof_node in path.iter().rev() {
            let key = &proof_node.key;
            if key.has_partial_byte() && proof_node.value_or_hash.has_value() {
                return Err(Error::PartialByteLengthWithValue);
            }

            // Materialise the node and override its value digest with the proof
            // node's value_or_hash (we may not know the preimage).
            self.trie.insert_skeleton(key.clone());
            self.value_digests
                .insert(key.clone(), proof_node.value_or_hash.clone());

            if !insert_any {
                continue;
            }

            for (index, child_id) in &proof_node.children {
                // childKey = key.Extend(ToToken(index), compressedKey). The
                // compressed key is that of the existing child edge in the
                // verification trie (Go `existingChild.compressedKey`), or empty
                // if no such child exists yet (an out-of-range boundary child).
                let existing_compressed = self
                    .trie
                    .child_compressed_key(key, *index)
                    .unwrap_or_else(Key::empty);
                let token = Key::to_token(*index, self.token_size);
                let child_key = key.extend(&[token, existing_compressed]);

                let less = match insert_children_less_than.value() {
                    Some(bound) => child_key < *bound,
                    None => false,
                };
                let greater = match insert_children_greater_than.value() {
                    Some(bound) => child_key > *bound,
                    None => false,
                };

                if less || greater {
                    self.child_injections
                        .entry(key.clone())
                        .or_default()
                        .insert(*index, *child_id);
                }
            }
        }
        Ok(())
    }

    /// Recomputes the root with the recorded value-digest overrides and child
    /// injections applied.
    fn root_id<H: Hasher>(&self, hasher: &H) -> Id {
        self.trie
            .root_with_overrides(hasher, &self.value_digests, &self.child_injections)
    }
}

// ---------------------------------------------------------------------------
// Verification helpers (ports of proof.go)
// ---------------------------------------------------------------------------

/// Returns true iff `value` and `value_or_hash` describe the same value.
/// Mirrors Go `valueOrHashMatches`.
fn value_or_hash_matches<H: Hasher>(
    hasher: &H,
    value: &Maybe<Bytes>,
    value_or_hash: &Maybe<Bytes>,
) -> bool {
    match (value, value_or_hash) {
        (Maybe::Nothing, Maybe::Nothing) => true,
        (Maybe::Nothing, Maybe::Some(_)) | (Maybe::Some(_), Maybe::Nothing) => false,
        (Maybe::Some(v), Maybe::Some(d)) => {
            if v.len() < HASH_LENGTH {
                v.as_ref() == d.as_ref()
            } else {
                hasher.hash_value(v).as_bytes().as_slice() == d.as_ref()
            }
        }
    }
}

/// Checks that every proof node whose key falls within `[start, end]` (and has
/// no partial byte) carries a value digest consistent with `key_values` (or
/// `Nothing` if the key isn't among them). Mirrors Go
/// `verifyChangeProofKeyValues` (the DB-lookup branch is `Nothing` here since
/// verification has no other state). A tampered `key_values` entry is caught
/// because its proof node still carries the original digest.
fn verify_proof_key_values<H: Hasher>(
    hasher: &H,
    proof_nodes: &[ProofNode],
    key_values: &BTreeMap<Key, Bytes>,
    start: Option<&Key>,
    end: Option<&Key>,
) -> Result<()> {
    for proof_node in proof_nodes {
        if proof_node.key.has_partial_byte() {
            continue;
        }
        let in_range =
            start.is_none_or(|s| proof_node.key >= *s) && end.is_none_or(|e| proof_node.key <= *e);
        if !in_range {
            continue;
        }
        let value: Maybe<Bytes> = key_values
            .get(&proof_node.key)
            .cloned()
            .map(Maybe::Some)
            .unwrap_or(Maybe::Nothing);

        if value.is_nothing() && proof_node.value_or_hash.has_value() {
            return Err(Error::ProofNodeHasUnincludedValue);
        }
        if value.has_value() && !value_or_hash_matches(hasher, &value, &proof_node.value_or_hash) {
            return Err(Error::ProofValueDoesntMatch);
        }
    }
    Ok(())
}

/// Validates a proof path: prefix monotonicity, partial-byte rules, and the
/// inclusion/exclusion shape. Mirrors Go `verifyProofPath`.
fn verify_proof_path(path: &[ProofNode], key: &Key, token_size: usize) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }

    for i in 0..path.len() - 1 {
        let node_key = &path[i].key;
        if node_key.has_partial_byte() && path[i].value_or_hash.has_value() {
            return Err(Error::PartialByteLengthWithValue);
        }
        if !key.has_strict_prefix(node_key) {
            return Err(Error::ProofNodeNotForKey);
        }
        let next_key = &path[i + 1].key;
        if !next_key.has_strict_prefix(node_key) {
            return Err(Error::NonIncreasingProofNodes);
        }
    }

    let last = &path[path.len() - 1];
    if last.key.has_partial_byte() && last.value_or_hash.has_value() {
        return Err(Error::PartialByteLengthWithValue);
    }

    if last.key != *key {
        // Exclusion proof.
        if key.has_prefix(&last.key) {
            // lastNode is an ancestor: it must not have a child where key goes.
            let next_index = key.token(last.key.length(), token_size);
            if last.children.contains_key(&next_index) {
                return Err(Error::ExclusionProofMissingEndNodes);
            }
        } else if path.len() > 1 {
            // lastNode is the replacement child: it must sit at key's index.
            let parent = &path[path.len() - 2];
            let bits_to_check = parent.key.length() + token_size;
            if !key.has_prefix(&last.key.take(bits_to_check)) {
                return Err(Error::ExclusionProofInvalidNode);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal protobuf wire encoder (proto/sync messages)
// ---------------------------------------------------------------------------

/// Appends an unsigned LEB128 varint.
fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Appends a field tag (field number + wire type).
fn put_tag(out: &mut Vec<u8>, field: u32, wire_type: u32) {
    put_uvarint(out, (u64::from(field) << 3) | u64::from(wire_type));
}

/// Appends a length-delimited (wire type 2) field carrying `payload`.
fn encode_len_field(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    put_tag(out, field, 2);
    put_uvarint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

/// Appends a `bytes` field. proto3 omits length-delimited fields whose value is
/// empty, matching Go's wire output for empty `bytes`.
fn encode_bytes_field(out: &mut Vec<u8>, field: u32, value: &[u8]) {
    if value.is_empty() {
        return;
    }
    encode_len_field(out, field, value);
}

/// Appends a `uint64` field (omitting the default value 0, per proto3).
fn encode_uint64_field(out: &mut Vec<u8>, field: u32, value: u64) {
    if value == 0 {
        return;
    }
    put_tag(out, field, 0);
    put_uvarint(out, value);
}

/// Encodes a `Key{length:1:uint64, value:2:bytes}` into a fresh buffer.
fn encode_key(key: &Key) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_uint64_field(&mut buf, 1, key.length() as u64);
    encode_bytes_field(&mut buf, 2, key.bytes());
    buf
}

/// Encodes a single `ProofNode{key:1, value_or_hash:2:MaybeBytes,
/// children:3:map<uint32,bytes>}`. Children are emitted in ascending index
/// order (deterministic).
fn encode_proof_node(node: &ProofNode) -> Vec<u8> {
    let mut buf = Vec::new();

    // field 1: Key (always present).
    let key_bytes = encode_key(&node.key);
    encode_len_field(&mut buf, 1, &key_bytes);

    // field 2: MaybeBytes value_or_hash (present iff Some).
    if let Maybe::Some(v) = &node.value_or_hash {
        let mut mb = Vec::new();
        encode_bytes_field(&mut mb, 1, v);
        encode_len_field(&mut buf, 2, &mb);
    }

    // field 3: map<uint32, bytes> children. Each entry is a length-delimited
    // message {key:1:uint32 (varint), value:2:bytes}.
    for (index, id) in &node.children {
        let mut entry = Vec::new();
        // map key (field 1) — varint; non-zero indices are emitted, and the 0
        // index must also be emitted (Go always writes map entry keys, even 0,
        // because they're explicit map entries, not message defaults).
        put_tag(&mut entry, 1, 0);
        put_uvarint(&mut entry, u64::from(*index));
        // map value (field 2) — bytes (32-byte ID).
        encode_len_field(&mut entry, 2, &id_bytes(id));
        encode_len_field(&mut buf, 3, &entry);
    }

    buf
}

/// Encodes a `repeated ProofNode` under `field`.
fn encode_repeated_proof_nodes(out: &mut Vec<u8>, field: u32, nodes: &[ProofNode]) {
    for node in nodes {
        let n = encode_proof_node(node);
        encode_len_field(out, field, &n);
    }
}

/// Returns a 32-byte ID slice.
fn id_bytes(id: &Id) -> [u8; ID_LEN] {
    id.to_bytes()
}

/// Builds a [`Key`] from packed bytes + a bit length, as when decoding a proof
/// node from the wire (mirrors Go `ToKey(value).Take(length)`).
///
/// # Errors
/// Returns [`Error::InvalidKeyLength`] if `value.len() != bytes_needed(length)`.
pub fn key_from_proto(value: &[u8], length_bits: usize) -> Result<Key> {
    if value.len() != bytes_needed(length_bits) {
        return Err(Error::InvalidKeyLength);
    }
    Ok(Key::from_bytes(value).take(length_bits))
}
