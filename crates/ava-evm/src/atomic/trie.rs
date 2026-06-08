// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The atomic trie — a SECOND, independent ethhash Firewood instance that
//! indexes the per-block atomic `Requests` keyed by `height(8B)||blockchainID`
//! (spec 10 §6.4/§17.4; coreth `plugin/evm/atomic/state/atomic_trie.go`).
//!
//! # What this is
//!
//! coreth maintains an `AtomicTrie` separate from the EVM state trie: for every
//! accepted block it inserts, for each peer chain touched by that block's atomic
//! txs, the key `[height: u64 big-endian][blockchainID: 32B]` (`TrieKeyLength =
//! LongLen + HashLength = 8 + 32 = 40`) mapping to `Codec.Marshal(0, requests)`
//! — the byte-exact avalanchego linear-codec serialization of that chain's
//! merged [`Requests`]. The trie root advances per accepted block and is
//! checkpointed (committed) at every `commitInterval` boundary
//! (`AtomicTrie.AcceptTrie`). We reproduce the root using a Firewood-ethhash trie
//! so it matches Go byte-for-byte.
//!
//! # As-built deviation (mirrors `state.rs`, spec 10 §17.2.2)
//!
//! `firewood::db::Proposal<'db>` borrows the `&Db` it was created from, so we
//! cannot stash a live proposal inside the struct that owns the `Db`. Instead we
//! build the deterministic [`FirewoodOps`] list, propose it against the tip to
//! read the resulting root, drop the borrow, then re-propose+commit the same ops
//! at commit time. The ops are fully deterministic so the recomputed root is
//! bit-identical.
//!
//! Unlike the EVM state trie (which commits exactly one proposal per block) the
//! atomic trie *accumulates* keys across blocks: each accepted block adds new
//! `(height, chain)` keys on top of the previously committed root. Because every
//! block uses a distinct `height` prefix, proposing the new block's ops against
//! the committed tip is the same as Go's "open trie at parent root, update,
//! commit" flow.

use std::collections::BTreeMap;
use std::path::Path;

use ava_codec::AvaCodec;
use ava_evm_reth::{B256, EMPTY_ROOT_HASH};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::Requests;
use firewood::api::{Db as _, DbView as _, Proposal as _};
use firewood::db::{BatchOp, Db, DbConfig};
use firewood::manager::RevisionManagerConfig;
use firewood_storage::NodeHashAlgorithm;

use crate::error::Error;
use crate::state::FirewoodOps;

/// `wrappers.LongLen` — the byte width of a packed `u64` (the height prefix).
pub const LONG_LEN: usize = 8;

/// `common.HashLength` — the byte width of a `blockchainID` (`ids.ID`).
pub const HASH_LENGTH: usize = 32;

/// `state.TrieKeyLength = wrappers.LongLen + common.HashLength = 40` — the fixed
/// width of an atomic-trie key (coreth `atomic_trie.go:31`).
pub const TRIE_KEY_LENGTH: usize = LONG_LEN + HASH_LENGTH;

/// Maps a [`firewood::api::Error`] into the C-Chain [`Error`] model.
fn map_fw_err(err: firewood::api::Error) -> Error {
    Error::Provider(ava_evm_reth::ProviderError::Database(
        ava_evm_reth::DatabaseError::Other(err.to_string()),
    ))
}

/// `chains/atomic.Requests` re-expressed as an `#[derive(AvaCodec)]` struct so
/// its serialization goes through the **atomic** linear codec byte-exactly
/// (coreth `AtomicTrie.UpdateTrie` does `Codec.Marshal(CodecVersion, requests)`).
///
/// Field order = serialization order (avalanchego `chains/atomic/shared_memory.go`):
/// `RemoveRequests [][]byte` then `PutRequests []*Element`. Each `[]byte` flows
/// through ava-codec's generic `Vec<T>`/`Vec<u8>` impls (`u32` count + raw bytes),
/// matching Go's reflectcodec slice handling.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
struct CodecRequests {
    /// `RemoveRequests` — keys to remove from the peer chain's shared memory.
    #[codec]
    remove: Vec<Vec<u8>>,
    /// `PutRequests` — elements to put into the peer chain's shared memory.
    #[codec]
    put: Vec<CodecElement>,
}

/// `chains/atomic.Element` mirrored for the atomic codec (`Key`, `Value`,
/// `Traits`, in serialization order).
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

impl From<&Requests> for CodecRequests {
    fn from(r: &Requests) -> Self {
        CodecRequests {
            remove: r.remove.clone(),
            put: r
                .put
                .iter()
                .map(|e| CodecElement {
                    key: e.key.clone(),
                    value: e.value.clone(),
                    traits: e.traits.clone(),
                })
                .collect(),
        }
    }
}

/// The atomic-trie key for `(height, blockchain_id)`:
/// `height.to_be_bytes()(8B) || blockchain_id(32B)`, total
/// [`TRIE_KEY_LENGTH`] = 40 bytes (coreth `AtomicTrie.UpdateTrie`,
/// `wrappers.Packer.PackLong` is big-endian).
#[must_use]
pub fn trie_key(height: u64, blockchain_id: &Id) -> [u8; TRIE_KEY_LENGTH] {
    let mut key = [0u8; TRIE_KEY_LENGTH];
    key[..LONG_LEN].copy_from_slice(&height.to_be_bytes());
    key[LONG_LEN..].copy_from_slice(&blockchain_id.to_bytes());
    key
}

/// Serializes a [`Requests`] to the byte-exact atomic-trie value
/// (`atomic.Codec.Marshal(CodecVersion, requests)` — a 2-byte version prefix
/// then `RemoveRequests`/`PutRequests`, coreth `AtomicTrie.UpdateTrie`).
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] if marshalling fails (effectively
/// impossible for the in-memory shared-memory payloads).
pub fn serialize_requests(requests: &Requests) -> ava_codec::error::Result<Vec<u8>> {
    let codec_requests = CodecRequests::from(requests);
    crate::atomic::tx::codec().marshal(crate::atomic::tx::CODEC_VERSION, &codec_requests)
}

/// The Firewood-ethhash atomic trie (a SECOND db, independent of the EVM state
/// trie). Holds the `Db`; commits advance the indexed root.
pub struct AtomicTrie {
    /// The ethhash (Keccak/Eth-MPT/RLP) atomic-index trie.
    db: Db,
}

impl AtomicTrie {
    /// Opens (creating if missing) an ethhash Firewood atomic trie at `dir`. This
    /// must be a different directory than the EVM state trie.
    ///
    /// # Errors
    /// Returns [`Error`] if Firewood fails to open the path or rejects the
    /// configuration.
    pub fn open(dir: impl AsRef<Path>) -> Result<AtomicTrie, Error> {
        let manager = RevisionManagerConfig::builder().max_revisions(256).build();
        let cfg = DbConfig::builder()
            .node_hash_algorithm(NodeHashAlgorithm::compile_option())
            .manager(manager)
            .build();
        let db = Db::new(dir.as_ref(), cfg).map_err(map_fw_err)?;
        Ok(AtomicTrie { db })
    }

    /// The current committed atomic-trie root (the ethhash empty-trie root
    /// `0x56e81f17…` == [`EMPTY_ROOT_HASH`] when no ops have been committed).
    #[must_use]
    pub fn root(&self) -> B256 {
        self.db
            .root_hash()
            .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()))
    }

    /// Builds the deterministic [`FirewoodOps`] for one block: for each peer
    /// chain in `atomic_ops` (sorted — a [`BTreeMap`], never a `HashMap` on a
    /// write path, spec 00 §6.1), one `Put` at `trie_key(height, chain)` of the
    /// serialized [`Requests`].
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if a [`Requests`] fails to
    /// serialize.
    pub fn ops_for_block(
        height: u64,
        atomic_ops: &BTreeMap<Id, Requests>,
    ) -> ava_codec::error::Result<FirewoodOps> {
        let mut ops: FirewoodOps = Vec::with_capacity(atomic_ops.len());
        for (chain, requests) in atomic_ops {
            let key = trie_key(height, chain).to_vec();
            let value = serialize_requests(requests)?;
            ops.push(BatchOp::Put { key, value });
        }
        Ok(ops)
    }

    /// Proposes `ops` against the committed tip and reads the resulting root
    /// **without committing** (the `verify`/`InsertTxs` shape).
    ///
    /// # Errors
    /// Returns [`Error`] on a Firewood propose/root failure.
    pub fn propose_root(&self, ops: FirewoodOps) -> Result<B256, Error> {
        let proposal = self.db.propose(ops).map_err(map_fw_err)?;
        let root = proposal
            .root_hash()
            .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()));
        Ok(root)
    }

    /// Commits `ops` against the committed tip, durably advancing the atomic-trie
    /// root, and returns the new root (the `AcceptTrie` shape). Because the ops
    /// are deterministic the committed root equals [`AtomicTrie::propose_root`]
    /// for the same ops.
    ///
    /// # Errors
    /// Returns [`Error`] if Firewood rejects the propose/commit.
    pub fn commit(&self, ops: FirewoodOps) -> Result<B256, Error> {
        let proposal = self.db.propose(ops).map_err(map_fw_err)?;
        let root = proposal
            .root_hash()
            .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()));
        proposal.commit().map_err(map_fw_err)?;
        Ok(root)
    }
}

#[cfg(test)]
mod tests {
    use ava_vm::components::avax::shared_memory::Element;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn trie_key_encoding_is_height_be_then_chain() {
        let chain = Id::from([0x22; 32]);
        let key = trie_key(1, &chain);
        assert_eq!(TRIE_KEY_LENGTH, 40);
        assert_eq!(
            hex::encode(key),
            "00000000000000012222222222222222222222222222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn serialize_requests_matches_go_remove() {
        // Go-executed golden (see tests/vectors/cchain/atomic_trie/_provenance.md):
        // Requests{remove:[input_id]} -> 00000001 00000020 <id> 00000000.
        let input_id =
            hex::decode("073baa2c7cbe84111ec1b5a2dba50afa546640f5f66ce3828be5c57ed9d77d93")
                .expect("hex");
        let reqs = Requests {
            remove: vec![input_id],
            put: Vec::new(),
        };
        assert_eq!(
            hex::encode(serialize_requests(&reqs).expect("serialize")),
            "00000000000100000020073baa2c7cbe84111ec1b5a2dba50afa546640f5f66ce3828be5c57ed9d77d9300000000"
        );
    }

    #[test]
    fn serialize_requests_matches_go_put() {
        // Go-executed golden: Requests{put:[Element{key,value,traits}]}.
        let key = hex::decode("c3da83f18816ccfe3294337d6d15188b13fc058de87d4b6778b15c2640993bca")
            .expect("hex");
        let value =
            hex::decode("000006ceeed2e0b93c5cb22055711767ce439ce220c94297136f64dd54438cd4fddc00000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000070000000000000bb8000000000000000000000001000000010505050505050505050505050505050505050505")
                .expect("hex");
        let trait_bytes = hex::decode("0505050505050505050505050505050505050505").expect("hex");
        let reqs = Requests {
            remove: Vec::new(),
            put: vec![Element {
                key,
                value,
                traits: vec![trait_bytes],
            }],
        };
        assert_eq!(
            hex::encode(serialize_requests(&reqs).expect("serialize")),
            "0000000000000000000100000020c3da83f18816ccfe3294337d6d15188b13fc058de87d4b6778b15c2640993bca00000076000006ceeed2e0b93c5cb22055711767ce439ce220c94297136f64dd54438cd4fddc00000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000070000000000000bb800000000000000000000000100000001050505050505050505050505050505050505050500000001000000140505050505050505050505050505050505050505"
        );
    }

    #[test]
    fn empty_trie_root_is_empty_root_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trie = AtomicTrie::open(dir.path()).expect("open");
        assert_eq!(trie.root(), EMPTY_ROOT_HASH);
    }
}
