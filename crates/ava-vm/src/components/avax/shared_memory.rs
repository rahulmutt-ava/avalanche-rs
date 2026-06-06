// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic UTXOs / cross-chain shared memory (`chains/atomic`, specs 07 §3.1).
//!
//! The [`SharedMemory`] trait is the per-chain view of cross-chain atomic
//! key/value/traits storage. The concrete implementation is owned by
//! `ava-chains` (and proxied over gRPC in §5); this module defines the trait +
//! the serializable [`Requests`] / [`Element`] payloads (`serialize:"true"`
//! fields, in field order).
//!
//! `apply` is keyed by a [`BTreeMap`] (never a `HashMap` on a write path —
//! specs 00 §6.1) so the per-chain request ordering is deterministic.

use std::collections::BTreeMap;

use ava_database::BatchOps;
use ava_types::id::Id;

use crate::error::Result;

/// The result of [`SharedMemory::indexed`]: `(values, last_trait, last_key)`.
pub type IndexedResult = (Vec<Vec<u8>>, Vec<u8>, Vec<u8>);

/// `atomic.Element` — a single atomic put: a key/value plus indexable traits.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Element {
    /// The element key (`serialize`).
    pub key: Vec<u8>,
    /// The element value (`serialize`).
    pub value: Vec<u8>,
    /// Indexable traits for `indexed` lookups (`serialize`).
    pub traits: Vec<Vec<u8>>,
}

/// `atomic.Requests` — the puts/removes to apply atomically for one peer chain.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Requests {
    /// Keys to remove (`RemoveRequests`, `serialize`).
    pub remove: Vec<Vec<u8>>,
    /// Elements to put (`PutRequests`, `serialize`).
    pub put: Vec<Element>,
}

/// `chains/atomic.SharedMemory` — a chain's view of cross-chain atomic storage.
///
/// Each chain operates on its own view, keyed by the peer chain id.
pub trait SharedMemory: Send + Sync {
    /// `Get(peerChainID, keys)` — fetch the values for `keys` sent from
    /// `peer_chain`. The result length equals `keys.len()`.
    ///
    /// # Errors
    /// Returns a [`crate::error::Error`] on a storage failure.
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>>;

    /// `Indexed(peerChainID, traits, startTrait, startKey, limit)` — paginate
    /// values matching any of `traits`, returning `(values, last_trait,
    /// last_key)` to resume from.
    ///
    /// # Errors
    /// Returns a [`crate::error::Error`] on a storage failure.
    fn indexed(
        &self,
        peer_chain: Id,
        traits: &[Vec<u8>],
        start_trait: &[u8],
        start_key: &[u8],
        limit: usize,
    ) -> Result<IndexedResult>;

    /// `Apply(requests, batches...)` — atomically apply the per-chain
    /// put/remove `requests` together with `batches` (which must share the
    /// underlying DB). Backs P/X/C atomic state writes.
    ///
    /// # Errors
    /// Returns a [`crate::error::Error`] if the atomic commit fails.
    fn apply(&self, requests: BTreeMap<Id, Requests>, batches: &[BatchOps]) -> Result<()>;
}
