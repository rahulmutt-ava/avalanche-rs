// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Hashing: the [`Hasher`] trait + the protocol-fixed SHA-256 [`DefaultHasher`].
//!
//! Byte-exact port of Go `x/merkledb/hashing.go`. The full `hash_node` ordering
//! and the in-memory trie-root computation are added in task M1.14; this file
//! currently provides the trait surface and `HashValue` (needed by the node
//! model's value-digest computation, M1.13).

use sha2::{Digest, Sha256};

use ava_types::id::Id;

/// The hash length in bytes. Mirrors Go `HashLength`.
pub const HASH_LENGTH: usize = 32;

/// A merkledb hasher. The protocol default is SHA-256 ([`DefaultHasher`]); the
/// trait exists only so the hash function is swappable in tests.
/// Mirrors Go `Hasher`.
pub trait Hasher {
    /// Returns the canonical hash of `value`. Mirrors Go `HashValue`.
    fn hash_value(&self, value: &[u8]) -> Id;
}

/// The protocol-fixed SHA-256 hasher. Mirrors Go `sha256Hasher` /
/// `DefaultHasher`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultHasher;

impl Hasher for DefaultHasher {
    fn hash_value(&self, value: &[u8]) -> Id {
        let mut sha = Sha256::new();
        sha.update(value);
        Id::from(<[u8; HASH_LENGTH]>::from(sha.finalize()))
    }
}
