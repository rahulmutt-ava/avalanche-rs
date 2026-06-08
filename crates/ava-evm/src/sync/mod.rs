// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EVM + atomic-trie state sync over Firewood range/inclusion proofs (G8,
//! spec 10 §10 / §17.9; 04 §4.2/§4.3). Populated by M6.25.
//!
//! # The G8 sync contract
//!
//! coreth state-sync (`vms/evm/sync`, `plugin/evm/syncervm`) lets a joining node
//! fetch the accepted EVM state at a recent block over **leaf** requests, plus
//! the **atomic trie** state, instead of replaying every block. We port the Go
//! protocol directly on top of Firewood proofs — reth's staged/snap sync is NOT
//! used (no engine, §1). The mapping (spec 10 §10):
//!
//! - **EVM account/storage state** → served from **Firewood range proofs** at a
//!   historical revision ([`server::EvmStateSyncServer`]); the client
//!   ([`client::StateSyncClient`]) reconstructs a fresh Firewood trie from the
//!   served leaves and verifies its root equals the target.
//! - **Atomic trie state** → synced the same way over the *second* Firewood
//!   instance (§6.4), then [`apply_atomic_trie_to_shared_memory`] replays the
//!   synced cursor's per-block `Requests` into shared memory.
//! - **Blocks/headers/receipts** → backfilled into [`crate::canonical::CanonicalStore`]
//!   via [`backfill_canonical`].
//!
//! # Wire format (byte-exact with the Go `firewood/syncer`)
//!
//! A leaf proof is Firewood's own `FrozenRangeProof` binary serialization
//! (`firewood::api::FrozenRangeProof::write_to_vec` / `from_slice`). The Go side
//! (`database/merkle/firewood/syncer`) serializes the identical bytes:
//! `(*ffi.RangeProof).MarshalBinary()` calls `fwd_range_proof_to_bytes`, a thin
//! cgo wrapper over the SAME firewood Rust serializer — so a proof produced here
//! deserializes/verifies on a Go node and vice-versa.
//!
//! The request/response *envelope* mirrors `proto/sync/sync.proto`:
//! `RangeProofRequest{root_hash, start_key, end_key, key_limit, bytes_limit}` and
//! `ProofResponse{range_proof: bytes}`. Here we carry the same fields as Rust
//! structs ([`LeafsRequest`] / [`LeafsResponse`]); the p2p SDK (specs/05) encodes
//! them with that proto schema at the wire boundary (12-node wiring).

pub mod client;
pub mod server;

use std::collections::BTreeMap;

use ava_codec::AvaCodec;
use ava_evm_reth::B256;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, Requests, SharedMemory};

use crate::atomic::trie::{HASH_LENGTH, LONG_LEN, TRIE_KEY_LENGTH};
use crate::canonical::CanonicalStore;
use crate::error::{Error, Result};
use crate::sync::client::StateSyncClient;

/// coreth's `sync.MaxKeyValuesLimit` — the server caps any leaf request at this
/// many key/value pairs regardless of the requested `key_limit`
/// (`database/merkle/sync/network_server.go`).
pub const MAX_KEY_VALUES_LIMIT: u32 = 2048;

/// The proto `MaybeBytes` (`proto/sync/sync.proto`): a bound key that is either
/// "nothing" (open end of the range) or "something" (an inclusive bound).
/// Mirrors avalanchego `maybe.Maybe[[]byte]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaybeBytes(Option<Vec<u8>>);

impl MaybeBytes {
    /// The "nothing" bound — an open end of the requested range.
    #[must_use]
    pub const fn nothing() -> Self {
        MaybeBytes(None)
    }

    /// A "something" bound carrying the given key bytes.
    #[must_use]
    pub fn some(key: impl Into<Vec<u8>>) -> Self {
        MaybeBytes(Some(key.into()))
    }

    /// The bound key bytes, or `None` when this is the open ("nothing") end.
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.0.as_deref()
    }

    /// `true` iff this is the open ("nothing") end of the range.
    #[must_use]
    pub fn is_nothing(&self) -> bool {
        self.0.is_none()
    }
}

/// A leaf (range-proof) request — the `RangeProofRequest` of `proto/sync/sync.proto`.
#[derive(Clone, Debug)]
pub struct LeafsRequest {
    /// The trie root to serve the proof at (a *historical* Firewood revision for
    /// EVM state; the atomic-trie root for the atomic instance).
    pub root: B256,
    /// Inclusive lower bound of the requested key range (open when nothing).
    pub start: MaybeBytes,
    /// Inclusive upper bound of the requested key range (open when nothing).
    pub end: MaybeBytes,
    /// Caller-requested max key/value pairs (capped at [`MAX_KEY_VALUES_LIMIT`]).
    pub key_limit: u32,
    /// Caller-requested max response bytes (advisory; Firewood truncates by
    /// `key_limit`, then the proof is range-verified regardless).
    pub bytes_limit: u32,
}

/// A leaf (range-proof) response — the `ProofResponse{range_proof}` of
/// `proto/sync/sync.proto`, with the served leaves split out for convenience.
///
/// `proof` is the byte-exact Firewood `FrozenRangeProof` serialization (wire
/// format shared with the Go syncer). `keys`/`vals` are the leaf key/value pairs
/// inside the proof, in ascending key order.
#[derive(Clone, Debug)]
pub struct LeafsResponse {
    /// The Firewood range-proof bytes (`FrozenRangeProof::write_to_vec`).
    pub proof: Vec<u8>,
    /// The proven keys, ascending.
    pub keys: Vec<Vec<u8>>,
    /// The proven values, parallel to `keys`.
    pub vals: Vec<Vec<u8>>,
}

/// `chains/atomic.Element` mirrored for the atomic codec — the value half of an
/// atomic-trie entry decodes back into these (the inverse of the encode path in
/// [`crate::atomic::trie`]).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
struct CodecElement {
    /// `Key` — the shared-memory element key.
    #[codec]
    key: Vec<u8>,
    /// `Value` — the shared-memory element value.
    #[codec]
    value: Vec<u8>,
    /// `Traits` — indexable traits for `indexed` lookups.
    #[codec]
    traits: Vec<Vec<u8>>,
}

/// `chains/atomic.Requests` mirrored for the atomic codec (decode side).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
struct CodecRequests {
    /// `RemoveRequests`.
    #[codec]
    remove: Vec<Vec<u8>>,
    /// `PutRequests`.
    #[codec]
    put: Vec<CodecElement>,
}

/// Deserializes an atomic-trie value (`atomic.Codec.Marshal(CodecVersion,
/// requests)`) back into a [`Requests`] — the inverse of
/// [`crate::atomic::trie::serialize_requests`].
///
/// # Errors
/// Returns [`Error::ConflictingAtomicInputs`] (the atomic-path error stand-in)
/// if the bytes are not a valid atomic-codec `Requests`.
fn deserialize_requests(bytes: &[u8]) -> Result<Requests> {
    let mut decoded = CodecRequests::default();
    crate::atomic::tx::codec()
        .unmarshal(bytes, &mut decoded)
        .map_err(|_| Error::ConflictingAtomicInputs)?;
    Ok(Requests {
        remove: decoded.remove,
        put: decoded
            .put
            .into_iter()
            .map(|e| Element {
                key: e.key,
                value: e.value,
                traits: e.traits,
            })
            .collect(),
    })
}

/// Parses an atomic-trie key (`height(8B) || blockchainID(32B)`,
/// [`TRIE_KEY_LENGTH`] bytes) back into `(height, chain)`.
fn parse_atomic_key(key: &[u8]) -> Option<(u64, Id)> {
    if key.len() != TRIE_KEY_LENGTH {
        return None;
    }
    let mut height_bytes = [0u8; LONG_LEN];
    height_bytes.copy_from_slice(&key[..LONG_LEN]);
    let mut chain_bytes = [0u8; HASH_LENGTH];
    chain_bytes.copy_from_slice(&key[LONG_LEN..]);
    Some((u64::from_be_bytes(height_bytes), Id::from(chain_bytes)))
}

/// `ApplyToSharedMemory` (coreth `AtomicBackend.ApplyToSharedMemory`): replay the
/// per-block atomic `Requests` indexed in a *synced* atomic trie into shared
/// memory, starting **after** `from_height` (exclusive). Returns the highest
/// height applied (the new cursor), or `from_height` if nothing was applied.
///
/// This is the startup reconcile a state-synced node runs once: after the atomic
/// trie is synced (over range proofs), the cross-chain effects it indexes have
/// not yet been pushed into this node's shared-memory half, so we walk the trie
/// in height order and apply each block's merged ops. Entries at or below
/// `from_height` are skipped (already applied / the sync cursor).
///
/// # Errors
/// Returns [`Error`] if a trie read, a value decode, or the shared-memory
/// [`SharedMemory::apply`] fails.
pub fn apply_atomic_trie_to_shared_memory(
    client: &StateSyncClient,
    shared_memory: &dyn SharedMemory,
    from_height: u64,
) -> Result<u64> {
    let view = client.view()?;

    // Collect (height -> chain -> Requests) in ascending height order. The atomic
    // trie keys sort `height_be || chain`, so trie order is height order.
    let mut by_height: BTreeMap<u64, BTreeMap<Id, Requests>> = BTreeMap::new();
    for (key, value) in view.iter_entries()? {
        let Some((height, chain)) = parse_atomic_key(&key) else {
            continue;
        };
        if height <= from_height {
            continue;
        }
        let requests = deserialize_requests(&value)?;
        by_height.entry(height).or_default().insert(chain, requests);
    }

    let mut cursor = from_height;
    for (height, ops) in by_height {
        if !ops.is_empty() {
            shared_memory
                .apply(ops, &[])
                .map_err(|_| Error::ConflictingAtomicInputs)?;
        }
        cursor = height;
    }
    Ok(cursor)
}

/// Backfills a synced block's non-state metadata (header commitment, body,
/// receipts) into the [`CanonicalStore`] (spec 10 §10 "block sync"; §17.7). This
/// is the seam a state-synced node uses to populate the block tables for the
/// accepted range, advancing the canonical tip strictly by `+1` (the store's
/// linearity invariant).
///
/// # Errors
/// Returns [`Error`] if the canonical append fails (e.g. a non-linear height).
pub fn backfill_canonical(
    store: &CanonicalStore,
    number: u64,
    hash: B256,
    header: B256,
    body: &[u8],
    receipts: &[u8],
) -> Result<()> {
    store.append_canonical(number, hash, header, body, receipts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_bytes_nothing_and_some() {
        assert!(MaybeBytes::nothing().is_nothing());
        assert_eq!(MaybeBytes::nothing().as_bytes(), None);
        let some = MaybeBytes::some(vec![1, 2, 3]);
        assert!(!some.is_nothing());
        assert_eq!(some.as_bytes(), Some([1, 2, 3].as_slice()));
    }

    #[test]
    fn parse_atomic_key_roundtrips() {
        let chain = Id::from([0x33; 32]);
        let key = crate::atomic::trie::trie_key(42, &chain);
        let (height, got) = parse_atomic_key(&key).expect("parse");
        assert_eq!(height, 42);
        assert_eq!(got, chain);
        // Wrong-length keys are ignored.
        assert_eq!(parse_atomic_key(&[0u8; 8]), None);
    }

    #[test]
    fn requests_codec_roundtrip() {
        let reqs = Requests {
            remove: vec![vec![0xaa; 32]],
            put: vec![Element {
                key: vec![0x01, 0x02],
                value: vec![0x03],
                traits: vec![vec![0x04]],
            }],
        };
        let bytes = crate::atomic::trie::serialize_requests(&reqs).expect("serialize");
        let decoded = deserialize_requests(&bytes).expect("deserialize");
        assert_eq!(decoded, reqs);
    }
}
