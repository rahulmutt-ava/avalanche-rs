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

/// Computes the merkle root ID of the trie holding exactly `kvs` under the given
/// `branch_factor`, using `hasher`. An empty `kvs` yields
/// [`ava_types::id::Id::EMPTY`]. Insertion order does not affect the result.
pub fn merkle_root<H: Hasher>(
    branch_factor: BranchFactor,
    hasher: &H,
    kvs: &[(&[u8], &[u8])],
) -> Id {
    let mut trie = crate::trie::Trie::new(branch_factor);
    for (k, v) in kvs {
        trie.apply(Key::from_bytes(k), Maybe::Some(Bytes::copy_from_slice(v)));
    }
    trie.root_id(hasher)
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
