// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Firewood-ethhash state backend: `FirewoodStateProvider` + views and the
//! `BundleState`->Firewood `BatchOp` conversion / `state_root*` provider (G1,
//! spec 10 §5/§17.2). Populated by M6.3/M6.4.
//!
//! # What this module owns (the 04 contract, spec 10 §5/§17.2)
//!
//! reth's [`StateProvider`](ava_evm_reth::StateProvider) is a super-trait bundle
//! (`AccountReader + BytecodeReader + BlockHashReader + StateRootProvider +
//! StorageRootProvider + StateProofProvider + HashedPostStateProvider`). By
//! supplying our own impl we route reads **and the post-execution state root**
//! through **Firewood in ethhash mode** (Keccak/Eth-MPT/RLP, spec 04 §4.2)
//! instead of reth's MPT-over-MDBX. Firewood is the EVM state-of-record;
//! reth-DB keeps only blocks/headers/receipts (spec 10 §5.1).
//!
//! Accounts and storage live in Firewood. Bytecode (`code_hash -> bytecode`)
//! and the block-hash window (`number -> hash`, for the `BLOCKHASH` opcode) live
//! in side KVs ([`ava_database`]), since they are not part of the trie.
//!
//! # The G1 trick (spec 10 §17.2 / §5.2)
//!
//! reth wants to compute the post-state root over its own MPT and persist trie
//! nodes via `StateWriter`. We must instead commit through Firewood. So
//! [`StateRootProvider::state_root_with_updates`](ava_evm_reth::StateRootProvider)
//! returns the **real Firewood root** but an **empty** [`TrieUpdates`] — reth
//! never persists trie nodes. The proposal that produced that root is stashed
//! (keyed by root) so `accept()` can commit exactly it; `reject()` drops it.
//!
//! ## As-built deviation from §17.2.2 (proposal stashing)
//!
//! The spec sketches stashing the live `firewood::db::Proposal` in a
//! `DashMap<B256, Proposal>`. `firewood::db::Proposal<'db>` borrows the `&Db`
//! it was created from, so storing it inside the same `FirewoodStateProvider`
//! that owns the `Db` is a self-referential borrow that safe Rust forbids.
//! Instead we stash the **deterministic [`BatchOp`] list** keyed by root, and at
//! `commit(root)` re-`propose` those ops against the current tip and commit.
//! Because the ops are fully deterministic the recomputed root is bit-identical,
//! so the contract ("commit exactly the state that produced this root") holds;
//! the only cost is one cheap in-memory re-propose. `#![forbid(unsafe_code)]`
//! is preserved.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use ava_database::{DynDatabase, Error as DbError};
use ava_evm_reth::{
    Account, AccountProof, Address, B256, BundleState, DatabaseError, EMPTY_ROOT_HASH,
    ExecutionWitnessMode, HashedPostState, HashedStorage, KeccakKeyHasher, MultiProof,
    MultiProofTargets, ProviderError, ProviderResult, RethBytecode, RlpDecodable, StorageKey,
    StorageMultiProof, StorageProof, StorageValue, TrieAccount, TrieInput, TrieUpdates, U256,
    keccak256, rlp_encode,
};
use firewood::api::{Db as _, DbView as _, HashKey, Proposal as _};
use firewood::db::{BatchOp, Db, DbConfig, Proposal};
use firewood::manager::RevisionManagerConfig;
use firewood_storage::NodeHashAlgorithm;
use parking_lot::Mutex;

use crate::error::Error;

/// Number of historical revisions Firewood retains (the reorg / state-sync
/// window — `db.revision(root)` can read any of the last `MAX_REVISIONS`
/// committed roots). Mirrors the Go node's firewood configuration.
const MAX_REVISIONS: usize = 256;

/// An owned-bytes Firewood batch op list — the deterministic conversion output
/// (§17.2.1) that we propose against the tip and stash by root (§17.2.2).
pub type FirewoodOps = Vec<BatchOp<Vec<u8>, Vec<u8>>>;

/// Maps a [`firewood::api::Error`] to a reth [`ProviderError`] so Firewood read
/// failures surface as the same "database" error reth would raise (spec 10
/// §11.2). A missing revision becomes
/// [`ProviderError::StateForHashNotFound`] elsewhere; here generic errors fold
/// into the database-error bucket.
fn map_fw_err(err: firewood::api::Error) -> ProviderError {
    ProviderError::Database(DatabaseError::Other(err.to_string()))
}

/// Wraps an arbitrary error string in a reth `DatabaseError::Other`.
fn db_other(msg: impl ToString) -> ProviderError {
    ProviderError::Database(DatabaseError::Other(msg.to_string()))
}

/// The Firewood-ethhash MPT key for an account leaf: `keccak256(addr)` at the
/// account depth (64 nibbles), per spec 10 §17.2 / 04 §4.2.
#[must_use]
pub fn account_key(addr: &Address) -> [u8; 32] {
    keccak256(addr).0
}

/// The Firewood-ethhash MPT key for a storage slot under `addr`: the account
/// path `keccak256(addr)` followed by the hashed slot `keccak256(slot)`
/// (spec 10 §17.2 / 04 §4.2). Firewood stores storage slots as children of the
/// account node, so the full key is the concatenation of the two hashes.
#[must_use]
pub fn storage_key(addr: &Address, slot: &B256) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(&keccak256(addr).0);
    key[32..].copy_from_slice(&keccak256(slot).0);
    key
}

/// The Firewood-ethhash storage key from already-hashed components (the
/// `HashedPostState` keys are `keccak256(addr)` / `keccak256(slot)` already).
#[must_use]
fn storage_node_key(hashed_addr: &B256, hashed_slot: &B256) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(hashed_addr.as_slice());
    key[32..].copy_from_slice(hashed_slot.as_slice());
    key
}

/// The Firewood-ethhash account key from an already-hashed address.
#[must_use]
fn account_node_key(hashed_addr: &B256) -> [u8; 32] {
    hashed_addr.0
}

/// The storage-trie prefix for an account (all of its slots share `keccak(addr)`
/// as the first 32 bytes), used for `DeleteRange` on a wiped account.
#[must_use]
fn storage_prefix(hashed_addr: &B256) -> Vec<u8> {
    hashed_addr.as_slice().to_vec()
}

/// Encodes an account leaf as the libevm `StateAccount` **5-field** RLP:
/// `[nonce, balance, storage_root, code_hash, is_multi_coin]` (spec 10
/// §17.2.1). The 5th field is coreth's `isMultiCoin` bool (always `false` =
/// `0x80` for standard C-Chain EOAs and contracts); it was added by
/// `ava-labs/libevm` to extend the consensus `StateAccount` representation.
///
/// Firewood-ethhash recomputes `storage_root` from the sub-trie at hash time,
/// so the value we write carries the empty-trie sentinel; the persisted RLP
/// shape is always the canonical 5-field list (spec 10 §17.2.1, M6.30).
///
/// ## Encoding note
///
/// The 4-field alloy `TrieAccount` encoding is `[0xf8, L, <payload>]` where
/// `L` is the 1-byte payload length (accounts are 68-108 bytes, always in
/// the 56-127 range that fits one byte). The 5-field encoding appends one
/// byte (`0x80` = `false`) to the payload and increments `L` by 1.
#[must_use]
pub fn rlp_account(nonce: u64, balance: U256, code_hash: B256) -> Vec<u8> {
    // Start from the standard 4-field alloy encoding.
    let mut bytes = rlp_encode(TrieAccount {
        nonce,
        balance,
        // Firewood derives the true storage_root from the children; we encode
        // the empty-trie sentinel for the leaf bytes (spec 10 §17.2.1).
        storage_root: EMPTY_ROOT_HASH,
        code_hash,
    });
    // Patch the list length: bytes[0] = 0xf8 (list with 1-byte length),
    // bytes[1] = payload_length. Add 1 for the extra bool field.
    // Safety: account payload is always 68-108 bytes < 255, so no overflow.
    debug_assert_eq!(
        bytes[0], 0xf8,
        "expected RLP list with 1-byte length prefix"
    );
    bytes[1] = bytes[1].saturating_add(1);
    // Append isMultiCoin=false as RLP bool `false` = 0x80 (alloy-rlp / libevm
    // `writeBool(false)` both produce 0x80, the empty-string sentinel for
    // zero/false; see rlp/doc.go and alloy-rlp/src/encode.rs).
    bytes.push(0x80);
    bytes
}

/// Decodes a libevm `StateAccount` **5-field** RLP blob into a reth [`Account`]
/// (`nonce`, `balance`, `bytecode_hash`). The `storage_root` and `is_multi_coin`
/// fields are dropped: reth's `Account` does not carry them (Firewood owns the
/// storage trie; `is_multi_coin` is always `false` on standard accounts). The
/// `code_hash` becomes `None` iff it is the empty-code sentinel `KECCAK_EMPTY`.
///
/// The decoder is forward-compatible: it reads the list header, decodes the
/// first 4 required fields, then ignores any remaining list payload (the
/// `is_multi_coin` 5th field and any future extensions).
///
/// # Errors
/// Returns [`ProviderError`] if the bytes are not valid 5-field account RLP or
/// if the list header is malformed.
pub fn decode_rlp_account(bytes: &[u8]) -> ProviderResult<Account> {
    // Parse the list header: accounts use `[0xf8, L]` (1-byte length prefix,
    // payload 68-109 bytes, always in the 56-127 range).
    if bytes.len() < 2 {
        return Err(db_other("account RLP too short"));
    }
    if bytes[0] != 0xf8 {
        return Err(db_other(format!(
            "expected RLP list header 0xf8, got 0x{:02x}",
            bytes[0]
        )));
    }
    let payload_len = bytes[1] as usize;
    let payload_end = payload_len
        .checked_add(2)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| db_other("account RLP payload truncated"))?;
    let mut payload: &[u8] = &bytes[2..payload_end];

    // Decode the 4 required fields (nonce, balance, storage_root, code_hash).
    let nonce = u64::decode(&mut payload).map_err(db_other)?;
    let balance = U256::decode(&mut payload).map_err(db_other)?;
    let _storage_root = B256::decode(&mut payload).map_err(db_other)?;
    let code_hash = B256::decode(&mut payload).map_err(db_other)?;
    // Any remaining payload bytes (the 5th `is_multi_coin` field and future
    // extensions) are intentionally ignored for forward-compatibility.

    let bytecode_hash = if code_hash == ava_evm_reth::KECCAK_EMPTY {
        None
    } else {
        Some(code_hash)
    };
    Ok(Account {
        nonce,
        balance,
        bytecode_hash,
    })
}

/// Encodes a storage slot value as `RLP(U256)` with leading zeros trimmed —
/// the standard Ethereum storage-leaf encoding (spec 10 §17.2.1).
#[must_use]
pub fn rlp_u256(value: U256) -> Vec<u8> {
    rlp_encode(value)
}

/// Decodes an `RLP(U256)` storage-slot value (spec 10 §17.2.1).
///
/// # Errors
/// Returns [`ProviderError`] if the bytes are not a valid RLP `U256`.
pub fn decode_rlp_u256(bytes: &[u8]) -> ProviderResult<StorageValue> {
    let mut buf = bytes;
    U256::decode(&mut buf).map_err(db_other)
}

/// Translates a [`HashedPostState`] (keccak-keyed account/storage delta) into a
/// deterministic list of Firewood ethhash [`BatchOp`]s (spec 10 §17.2.1 — the
/// single most correctness-critical conversion in the port).
///
/// Ordering: **storage first** (so an account's storage_root reflects updated
/// slots), then accounts — both in sorted key order for reproducibility (the
/// Firewood root is order-independent, but sorting eases golden-vector
/// debugging). Zero-valued slots and destroyed accounts become `Delete`; a
/// `wiped` storage becomes a `DeleteRange` over the account prefix.
#[must_use]
pub fn hashed_post_state_to_batchops(h: &HashedPostState) -> FirewoodOps {
    let mut ops: FirewoodOps = Vec::with_capacity(h.accounts.len() + h.storages.len() * 4);

    // 1. Storage first, in sorted (hashed_addr, hashed_slot) order.
    let mut storages: Vec<(&B256, &HashedStorage)> = h.storages.iter().collect();
    storages.sort_by(|a, b| a.0.cmp(b.0));
    for (hashed_addr, storage) in storages {
        if storage.wiped {
            ops.push(BatchOp::DeleteRange {
                prefix: storage_prefix(hashed_addr),
            });
        }
        let mut slots: Vec<(&B256, &U256)> = storage.storage.iter().collect();
        slots.sort_by(|a, b| a.0.cmp(b.0));
        for (hashed_slot, value) in slots {
            let key = storage_node_key(hashed_addr, hashed_slot).to_vec();
            if value.is_zero() {
                ops.push(BatchOp::Delete { key });
            } else {
                ops.push(BatchOp::Put {
                    key,
                    value: rlp_u256(*value),
                });
            }
        }
    }

    // 2. Accounts, in sorted hashed_addr order.
    let mut accounts: Vec<(&B256, &Option<Account>)> = h.accounts.iter().collect();
    accounts.sort_by(|a, b| a.0.cmp(b.0));
    for (hashed_addr, account) in accounts {
        match account {
            None => ops.push(BatchOp::Delete {
                key: account_node_key(hashed_addr).to_vec(),
            }),
            Some(acc) => ops.push(BatchOp::Put {
                key: account_node_key(hashed_addr).to_vec(),
                value: rlp_account(acc.nonce, acc.balance, acc.get_bytecode_hash()),
            }),
        }
    }

    ops
}

/// Backs reth state reads/roots with Firewood in ethhash mode (spec 10 §5,
/// §17.2; 04 §4.2/§4.3 is the contract).
///
/// Opens read views at any retained revision and owns the propose -> stash ->
/// commit lifecycle. Accounts/storage are Firewood; bytecode and block hashes
/// are side KVs.
pub struct FirewoodStateProvider {
    /// The ethhash (Keccak/Eth-MPT/RLP) state-of-record trie (spec 04 §4.2).
    db: Db,
    /// `code_hash -> bytecode` (not in the trie, spec 10 §5.1).
    bytecode: Arc<dyn DynDatabase>,
    /// `number -> hash` for the `BLOCKHASH` opcode window (spec 10 §5.1).
    block_hashes: Arc<dyn DynDatabase>,
    /// verify()->accept() proposal stash, keyed by post-state root. We stash the
    /// deterministic ops (see the module-level as-built note) rather than the
    /// borrowed `Proposal`.
    stash: Mutex<BTreeMap<B256, FirewoodOps>>,
}

impl FirewoodStateProvider {
    /// Opens (creating if missing) an ethhash Firewood database at `dir`, with
    /// the given bytecode and block-hash side stores.
    ///
    /// # Errors
    /// Returns [`Error`] if Firewood fails to open the path or rejects the
    /// configuration (e.g. a hash-mode mismatch on an existing database).
    pub fn open(
        dir: impl AsRef<Path>,
        bytecode: Arc<dyn DynDatabase>,
        block_hashes: Arc<dyn DynDatabase>,
    ) -> Result<Arc<FirewoodStateProvider>, Error> {
        let manager = RevisionManagerConfig::builder()
            .max_revisions(MAX_REVISIONS)
            .build();
        let cfg = DbConfig::builder()
            // Bind the runtime mode to the compiled feature so Firewood never
            // rejects us for a hash-algorithm mismatch (ethhash is global).
            .node_hash_algorithm(NodeHashAlgorithm::compile_option())
            .manager(manager)
            .build();
        let db = Db::new(dir.as_ref(), cfg).map_err(|e| Error::Provider(map_fw_err(e)))?;
        Ok(Arc::new(FirewoodStateProvider {
            db,
            bytecode,
            block_hashes,
            stash: Mutex::new(BTreeMap::new()),
        }))
    }

    /// The current committed EVM state root (the ethhash empty-trie root
    /// `0x56e81f17…` when no state is committed).
    #[must_use]
    pub fn root(&self) -> B256 {
        self.db
            .root_hash()
            .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()))
    }

    /// A read view pinned at the current tip (latest committed revision).
    ///
    /// # Errors
    /// Returns [`ProviderError::StateForHashNotFound`] if the tip revision is
    /// somehow not retained, or a database error from Firewood.
    pub fn view_tip(self: &Arc<Self>) -> ProviderResult<FirewoodStateView> {
        let root = self.root();
        self.history_by_state_root(root)
    }

    /// A read view pinned at the committed revision identified by `root` (the
    /// G2 history window of spec 10 §5.2 — outside the window maps to coreth's
    /// "pruned/unavailable" error).
    ///
    /// # Errors
    /// Returns [`ProviderError::StateForHashNotFound`] when `root` is no longer
    /// in the retained revision window.
    pub fn history_by_state_root(
        self: &Arc<Self>,
        root: B256,
    ) -> ProviderResult<FirewoodStateView> {
        let hash = HashKey::try_from(root.as_slice())
            .map_err(|_| ProviderError::StateForHashNotFound(root))?;
        let rev = self
            .db
            .revision(hash)
            .map_err(|_| ProviderError::StateForHashNotFound(root))?;
        Ok(FirewoodStateView {
            rev,
            provider: Arc::clone(self),
        })
    }

    /// Computes the post-state root for `bundle` (translate -> propose -> read)
    /// without committing, and stashes the proposal ops keyed by that root.
    ///
    /// # Errors
    /// Returns [`ProviderError`] on a Firewood propose/root failure.
    pub fn propose_from_bundle(&self, bundle: &BundleState) -> ProviderResult<B256> {
        let hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(&bundle.state);
        let ops = hashed_post_state_to_batchops(&hashed);
        self.propose_and_stash(ops)
    }

    /// Builds a proposal over `ops` against the tip, reads its root, stashes the
    /// ops by root, and returns the root. Shared by `state_root_with_updates`
    /// and `propose_from_bundle`.
    fn propose_and_stash(&self, ops: FirewoodOps) -> ProviderResult<B256> {
        let proposal = self.db.propose(ops.clone()).map_err(map_fw_err)?;
        let root = proposal_root(&proposal);
        // Drop the borrowed proposal; we stash ops and re-propose at commit.
        drop(proposal);
        self.stash_proposal(root, ops);
        Ok(root)
    }

    /// Stashes the deterministic proposal ops keyed by post-state `root` so
    /// `commit(root)` can apply exactly this state (spec 10 §17.2.2).
    pub fn stash_proposal(&self, root: B256, ops: FirewoodOps) {
        self.stash.lock().insert(root, ops);
    }

    /// Removes and returns the stashed ops for `root`, if any.
    #[must_use]
    pub fn take_stashed(&self, root: B256) -> Option<FirewoodOps> {
        self.stash.lock().remove(&root)
    }

    /// Commits the stashed proposal for `root`, durably advancing the EVM tip
    /// (spec 10 §17.2.2 / 04 §4.2). No recompute of unrelated state.
    ///
    /// # Errors
    /// Returns [`Error::MissingProposal`] if no proposal is stashed for `root`,
    /// or [`Error::Provider`] if Firewood rejects the propose/commit.
    pub fn commit(&self, root: B256) -> Result<(), Error> {
        let ops = self
            .take_stashed(root)
            .ok_or(Error::MissingProposal(root))?;
        let proposal = self
            .db
            .propose(ops)
            .map_err(|e| Error::Provider(map_fw_err(e)))?;
        proposal
            .commit()
            .map_err(|e| Error::Provider(map_fw_err(e)))?;
        Ok(())
    }

    /// Discards the stashed proposal for `root` (reject is free — drop the ops).
    pub fn discard(&self, root: B256) {
        let _ = self.take_stashed(root);
    }

    /// Borrows the bytecode side store (`code_hash -> bytecode`).
    #[must_use]
    pub fn bytecode_store(&self) -> &Arc<dyn DynDatabase> {
        &self.bytecode
    }

    /// Borrows the block-hash side store (`number -> hash`).
    #[must_use]
    pub fn block_hash_store(&self) -> &Arc<dyn DynDatabase> {
        &self.block_hashes
    }
}

/// Reads the root of a Firewood proposal as a [`B256`] (the ethhash empty-trie
/// root when the proposal is empty).
fn proposal_root(proposal: &Proposal<'_>) -> B256 {
    proposal
        .root_hash()
        .map_or(EMPTY_ROOT_HASH, |h| B256::from_slice(h.as_ref()))
}

/// A read view pinned at one Firewood revision (parent / historical / tip),
/// spec 10 §17.2.
pub struct FirewoodStateView {
    /// The pinned committed revision (`db.revision(root)`).
    rev: Arc<<Db as firewood::api::Db>::Historical>,
    /// Back-reference for code reads and propose/commit (the provider owns the
    /// side KVs and the stash).
    provider: Arc<FirewoodStateProvider>,
}

impl FirewoodStateView {
    /// Reads a raw value from the pinned revision.
    fn val(&self, key: &[u8]) -> ProviderResult<Option<Vec<u8>>> {
        Ok(self.rev.val(key).map_err(map_fw_err)?.map(|v| v.to_vec()))
    }
}

impl ava_evm_reth::AccountReader for FirewoodStateView {
    fn basic_account(&self, addr: &Address) -> ProviderResult<Option<Account>> {
        match self.val(&account_key(addr))? {
            Some(rlp) => Ok(Some(decode_rlp_account(&rlp)?)),
            None => Ok(None),
        }
    }
}

impl ava_evm_reth::StateProvider for FirewoodStateView {
    fn storage(
        &self,
        account: Address,
        storage_key: StorageKey,
    ) -> ProviderResult<Option<StorageValue>> {
        match self.val(&storage_key_bytes(&account, &storage_key))? {
            Some(rlp) => Ok(Some(decode_rlp_u256(&rlp)?)),
            None => Ok(None),
        }
    }
}

/// Storage key bytes from an `(addr, slot)` pair (`StorageKey` is `B256`).
fn storage_key_bytes(addr: &Address, slot: &StorageKey) -> [u8; 64] {
    storage_key(addr, slot)
}

impl ava_evm_reth::BytecodeReader for FirewoodStateView {
    fn bytecode_by_hash(&self, code_hash: &B256) -> ProviderResult<Option<RethBytecode>> {
        match self.provider.bytecode.get(code_hash.as_slice()) {
            Ok(bytes) => Ok(Some(RethBytecode::new_raw(bytes.into()))),
            Err(DbError::NotFound) => Ok(None),
            Err(e) => Err(db_other(e)),
        }
    }
}

impl ava_evm_reth::BlockHashReader for FirewoodStateView {
    fn block_hash(&self, number: u64) -> ProviderResult<Option<B256>> {
        match self.provider.block_hashes.get(&number.to_be_bytes()) {
            Ok(bytes) if bytes.len() == 32 => Ok(Some(B256::from_slice(&bytes))),
            Ok(_) => Ok(None),
            Err(DbError::NotFound) => Ok(None),
            Err(e) => Err(db_other(e)),
        }
    }

    fn canonical_hashes_range(&self, start: u64, end: u64) -> ProviderResult<Vec<B256>> {
        let mut hashes = Vec::new();
        for number in start..end {
            if let Some(h) = self.block_hash(number)? {
                hashes.push(h);
            }
        }
        Ok(hashes)
    }
}

impl ava_evm_reth::HashedPostStateProvider for FirewoodStateView {
    fn hashed_post_state(&self, bundle_state: &BundleState) -> HashedPostState {
        // Our keccak hashing MUST match Firewood-ethhash key derivation exactly
        // so the HashedPostState keys line up with our BatchOps keys.
        HashedPostState::from_bundle_state::<KeccakKeyHasher>(&bundle_state.state)
    }
}

impl ava_evm_reth::StateRootProvider for FirewoodStateView {
    fn state_root(&self, hashed_state: HashedPostState) -> ProviderResult<B256> {
        // Pre-commit root only — translate to BatchOps, propose against the tip,
        // read the root. (We propose against the db tip, mirroring reth's
        // "on top of the current state" contract.)
        let ops = hashed_post_state_to_batchops(&hashed_state);
        let proposal = self.provider.db.propose(ops).map_err(map_fw_err)?;
        Ok(proposal_root(&proposal))
    }

    fn state_root_from_nodes(&self, input: TrieInput) -> ProviderResult<B256> {
        // reth's intermediate-node fast path is meaningless for Firewood; fold
        // the TrieInput's HashedPostState in and ignore the prefix-set/nodes.
        self.state_root(input.state)
    }

    fn state_root_with_updates(
        &self,
        hashed_state: HashedPostState,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        // The real Firewood root + an EMPTY TrieUpdates: reth must never persist
        // trie nodes (Firewood is state-of-record, the G1 invariant). The
        // proposal ops are stashed by root so accept() can commit exactly them.
        let ops = hashed_post_state_to_batchops(&hashed_state);
        let root = self.provider.propose_and_stash(ops)?;
        Ok((root, TrieUpdates::default()))
    }

    fn state_root_from_nodes_with_updates(
        &self,
        input: TrieInput,
    ) -> ProviderResult<(B256, TrieUpdates)> {
        self.state_root_with_updates(input.state)
    }
}

impl ava_evm_reth::StorageRootProvider for FirewoodStateView {
    fn storage_root(
        &self,
        _address: Address,
        _hashed_storage: ava_evm_reth::HashedStorage,
    ) -> ProviderResult<B256> {
        // Firewood-ethhash recomputes per-account storage roots internally at
        // hash time; a standalone sub-trie root over an ad-hoc HashedStorage is
        // not exposed by the firewood v0.5 API. eth_getProof / sub-trie roots
        // are M6.25 (state sync) scope; return the empty-trie sentinel until
        // then so callers that only need a placeholder are unblocked.
        Ok(EMPTY_ROOT_HASH)
    }

    fn storage_proof(
        &self,
        _address: Address,
        _slot: B256,
        _hashed_storage: ava_evm_reth::HashedStorage,
    ) -> ProviderResult<StorageProof> {
        Err(unsupported("storage_proof"))
    }

    fn storage_multiproof(
        &self,
        _address: Address,
        _slots: &[B256],
        _hashed_storage: ava_evm_reth::HashedStorage,
    ) -> ProviderResult<StorageMultiProof> {
        Err(unsupported("storage_multiproof"))
    }
}

impl ava_evm_reth::StateProofProvider for FirewoodStateView {
    fn proof(
        &self,
        _input: TrieInput,
        _address: Address,
        _slots: &[B256],
    ) -> ProviderResult<AccountProof> {
        Err(unsupported("proof"))
    }

    fn multiproof(
        &self,
        _input: TrieInput,
        _targets: MultiProofTargets,
    ) -> ProviderResult<MultiProof> {
        Err(unsupported("multiproof"))
    }

    fn witness(
        &self,
        _input: TrieInput,
        _target: HashedPostState,
        _mode: ExecutionWitnessMode,
    ) -> ProviderResult<Vec<ava_evm_reth::Bytes>> {
        Err(unsupported("witness"))
    }
}

/// A `ProviderError` for proof/witness paths not yet wired to Firewood proofs
/// (M6.25 state-sync scope, spec 10 §10/§17.9).
fn unsupported(what: &str) -> ProviderError {
    db_other(format!(
        "firewood {what} not yet implemented (M6.25 state-sync scope)"
    ))
}

#[cfg(test)]
mod tests {
    use ava_database::MemDb;
    use ava_evm_reth::{
        AccountReader, BlockHashReader, BytecodeReader, HashedStorage, StateProvider,
        StateRootProvider,
    };
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    fn side_stores() -> (Arc<dyn DynDatabase>, Arc<dyn DynDatabase>) {
        (Arc::new(MemDb::new()), Arc::new(MemDb::new()))
    }

    fn open_provider() -> (tempfile::TempDir, Arc<FirewoodStateProvider>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (bytecode, block_hashes) = side_stores();
        let provider =
            FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open");
        (dir, provider)
    }

    #[test]
    fn read_account_and_storage_roundtrip() {
        let (_dir, provider) = open_provider();

        let addr = Address::repeat_byte(0x11);
        let slot = B256::repeat_byte(0x22);
        let slot_value = U256::from(0xdead_beef_u64);
        let code = vec![0x60, 0x00, 0x60, 0x00];
        let code_hash = keccak256(&code);

        // Seed the bytecode side store.
        provider
            .bytecode
            .put(code_hash.as_slice(), &code)
            .expect("put code");
        // Seed a block hash.
        let block_hash = B256::repeat_byte(0x42);
        provider
            .block_hashes
            .put(&7u64.to_be_bytes(), block_hash.as_slice())
            .expect("put blockhash");

        // Build a HashedPostState with one account + one storage slot and commit
        // it through the provider's propose/stash/commit lifecycle.
        let hashed_addr = keccak256(addr);
        let hashed_slot = keccak256(slot);
        let mut storages = ava_evm_reth::B256Map::default();
        let mut slots = ava_evm_reth::B256Map::default();
        slots.insert(hashed_slot, slot_value);
        storages.insert(
            hashed_addr,
            HashedStorage {
                wiped: false,
                storage: slots,
            },
        );
        let mut accounts = ava_evm_reth::B256Map::default();
        accounts.insert(
            hashed_addr,
            Some(Account {
                nonce: 5,
                balance: U256::from(1_000_000_u64),
                bytecode_hash: Some(code_hash),
            }),
        );
        let hashed = HashedPostState { accounts, storages };

        let ops = hashed_post_state_to_batchops(&hashed);
        let root = provider.propose_and_stash(ops).expect("stash");
        provider.commit(root).expect("commit");
        assert_eq!(provider.root(), root);

        // Read back via a tip view.
        let view = provider.view_tip().expect("view");
        let acct = view
            .basic_account(&addr)
            .expect("read account")
            .expect("present");
        assert_eq!(acct.nonce, 5);
        assert_eq!(acct.balance, U256::from(1_000_000_u64));
        assert_eq!(acct.bytecode_hash, Some(code_hash));

        let read_slot = view
            .storage(addr, slot)
            .expect("read storage")
            .expect("present");
        assert_eq!(read_slot, slot_value);

        let bytecode = view
            .bytecode_by_hash(&code_hash)
            .expect("read code")
            .expect("present");
        assert_eq!(bytecode.original_bytes().as_ref(), code.as_slice());

        let bh = view
            .block_hash(7)
            .expect("read blockhash")
            .expect("present");
        assert_eq!(bh, block_hash);

        // Absent reads return None.
        assert_eq!(
            view.basic_account(&Address::repeat_byte(0x99))
                .expect("read"),
            None
        );
    }

    #[test]
    fn decode_rlp_account_roundtrip() {
        // Golden vector: libevm 5-field StateAccount RLP [nonce, balance,
        // storage_root, code_hash, isMultiCoin=false]. Provenance documented in
        // tests/vectors/cchain/account_rlp/_provenance.md.
        let nonce = 7u64;
        let balance = U256::from(0x0de0_b6b3_a764_0000_u128); // 1 ether in wei
        let code_hash = keccak256([0xaa, 0xbb, 0xcc]);

        let encoded = rlp_account(nonce, balance, code_hash);
        let decoded = decode_rlp_account(&encoded).expect("decode");
        assert_eq!(decoded.nonce, nonce);
        assert_eq!(decoded.balance, balance);
        assert_eq!(decoded.bytecode_hash, Some(code_hash));

        // Empty-code account decodes to bytecode_hash == None.
        let empty = rlp_account(0, U256::ZERO, ava_evm_reth::KECCAK_EMPTY);
        let decoded = decode_rlp_account(&empty).expect("decode");
        assert_eq!(decoded.bytecode_hash, None);
    }

    #[test]
    fn account_rlp_golden_vector() {
        // The committed golden vector must decode to the documented account.
        let raw = include_str!("../tests/vectors/cchain/account_rlp/eoa_one_ether.json");
        let v: serde_json::Value = serde_json::from_str(raw).expect("json");
        let rlp_hex = v["rlp"].as_str().expect("rlp field");
        let rlp = hex::decode(rlp_hex.trim_start_matches("0x")).expect("hex");

        let decoded = decode_rlp_account(&rlp).expect("decode");
        let want_nonce = v["nonce"].as_u64().expect("nonce");
        let want_balance =
            U256::from_str_radix(v["balance"].as_str().expect("balance"), 10).expect("balance");
        assert_eq!(decoded.nonce, want_nonce);
        assert_eq!(decoded.balance, want_balance);

        // Re-encoding the documented fields reproduces the golden bytes.
        let reencoded = rlp_account(want_nonce, want_balance, ava_evm_reth::KECCAK_EMPTY);
        assert_eq!(hex::encode(&reencoded), rlp_hex.trim_start_matches("0x"));
    }

    #[test]
    fn hashed_post_state_to_batchops_is_deterministic() {
        // Build a state with two accounts (each with two slots) and assert the
        // ops are storage-before-accounts, sorted, with zero-slot -> Delete and
        // None-account -> Delete.
        let addr_a = keccak256(Address::repeat_byte(0x01));
        let addr_b = keccak256(Address::repeat_byte(0x02));
        let slot_x = keccak256(B256::repeat_byte(0x0a));
        let slot_y = keccak256(B256::repeat_byte(0x0b));

        let make = || {
            let mut storages = ava_evm_reth::B256Map::default();
            let mut slots_a = ava_evm_reth::B256Map::default();
            slots_a.insert(slot_x, U256::from(1u64));
            slots_a.insert(slot_y, U256::ZERO); // -> Delete
            storages.insert(
                addr_a,
                HashedStorage {
                    wiped: false,
                    storage: slots_a,
                },
            );
            let mut slots_b = ava_evm_reth::B256Map::default();
            slots_b.insert(slot_x, U256::from(9u64));
            storages.insert(
                addr_b,
                HashedStorage {
                    wiped: true, // -> DeleteRange first
                    storage: slots_b,
                },
            );

            let mut accounts = ava_evm_reth::B256Map::default();
            accounts.insert(
                addr_a,
                Some(Account {
                    nonce: 1,
                    balance: U256::from(10u64),
                    bytecode_hash: None,
                }),
            );
            accounts.insert(addr_b, None); // -> Delete
            HashedPostState { accounts, storages }
        };

        // Determinism: building from two independently-constructed (hash-ordered)
        // maps yields the identical op stream.
        let ops1 = hashed_post_state_to_batchops(&make());
        let ops2 = hashed_post_state_to_batchops(&make());
        assert_eq!(ops1, ops2);

        // All storage ops precede all account ops.
        let first_account = ops1
            .iter()
            .position(|op| match op {
                BatchOp::Put { key, .. } | BatchOp::Delete { key } => key.len() == 32,
                _ => false,
            })
            .expect("an account op");
        for op in &ops1[..first_account] {
            let is_storage = match op {
                BatchOp::Put { key, .. } | BatchOp::Delete { key } => key.len() == 64,
                BatchOp::DeleteRange { .. } => true,
                _ => false,
            };
            assert!(is_storage, "storage ops must precede account ops");
        }

        // A wiped account emits a DeleteRange.
        assert!(
            ops1.iter()
                .any(|op| matches!(op, BatchOp::DeleteRange { .. }))
        );
        // The None account emits a Delete on a 32-byte key.
        assert!(ops1.iter().any(|op| matches!(
            op,
            BatchOp::Delete { key } if key.as_slice() == addr_b.as_slice()
        )));
        // The zero slot emits a Delete on a 64-byte key.
        let zero_key = storage_node_key(&addr_a, &slot_y).to_vec();
        assert!(
            ops1.iter()
                .any(|op| matches!(op, BatchOp::Delete { key } if *key == zero_key))
        );
    }

    #[test]
    fn stash_then_commit_advances_tip() {
        let (_dir, provider) = open_provider();
        let before = provider.root();

        let addr = keccak256(Address::repeat_byte(0x33));
        let mut accounts = ava_evm_reth::B256Map::default();
        accounts.insert(
            addr,
            Some(Account {
                nonce: 1,
                balance: U256::from(42u64),
                bytecode_hash: None,
            }),
        );
        let hashed = HashedPostState {
            accounts,
            storages: ava_evm_reth::B256Map::default(),
        };

        let view = provider.view_tip().expect("view");
        let (root, updates) = view.state_root_with_updates(hashed).expect("root");

        // G1 trick: empty TrieUpdates.
        assert!(updates.is_empty(), "TrieUpdates must be empty (G1)");
        // The proposal is stashed by root and not yet committed (tip unchanged).
        assert_eq!(provider.root(), before);
        assert_ne!(root, before);

        // Commit advances the tip to exactly that root.
        provider.commit(root).expect("commit");
        assert_eq!(provider.root(), root);
        // The stash is drained.
        assert_eq!(provider.take_stashed(root), None);
    }

    #[test]
    fn discard_drops_stashed_proposal() {
        let (_dir, provider) = open_provider();
        let mut accounts = ava_evm_reth::B256Map::default();
        accounts.insert(
            keccak256(Address::repeat_byte(0x44)),
            Some(Account {
                nonce: 1,
                balance: U256::from(1u64),
                bytecode_hash: None,
            }),
        );
        let hashed = HashedPostState {
            accounts,
            storages: ava_evm_reth::B256Map::default(),
        };
        let view = provider.view_tip().expect("view");
        let (root, _) = view.state_root_with_updates(hashed).expect("root");
        provider.discard(root);
        assert_eq!(provider.take_stashed(root), None);
        // Commit after discard fails with MissingProposal.
        assert_matches::assert_matches!(provider.commit(root), Err(Error::MissingProposal(_)));
    }

    proptest! {
        /// The Firewood ethhash root is independent of insertion order: the same
        /// K/V set, committed in any order, yields the same root (04 §4.2
        /// merkledb invariant, applied to ethhash).
        #[test]
        fn prop_state_root_order_independent(
            mut entries in proptest::collection::vec(
                (any::<[u8; 20]>(), any::<u64>(), 1u64..=u64::MAX),
                1..8usize,
            )
        ) {
            // Dedup by address (a HashedPostState has one entry per address).
            entries.sort_by_key(|(a, _, _)| *a);
            entries.dedup_by_key(|(a, _, _)| *a);

            let build_state = |order: &[([u8; 20], u64, u64)]| -> HashedPostState {
                let mut accounts = ava_evm_reth::B256Map::default();
                for (addr, nonce, bal) in order {
                    accounts.insert(
                        keccak256(Address::from(*addr)),
                        Some(Account {
                            nonce: *nonce,
                            balance: U256::from(*bal),
                            bytecode_hash: None,
                        }),
                    );
                }
                HashedPostState { accounts, storages: ava_evm_reth::B256Map::default() }
            };

            let (_dir1, p1) = open_provider();
            let (_dir2, p2) = open_provider();

            let forward = build_state(&entries);
            let mut rev: Vec<_> = entries.clone();
            rev.reverse();
            let backward = build_state(&rev);

            let v1 = p1.view_tip().expect("view");
            let r1 = v1.state_root(forward).expect("root1");
            let v2 = p2.view_tip().expect("view");
            let r2 = v2.state_root(backward).expect("root2");
            prop_assert_eq!(r1, r2);
        }
    }
}
