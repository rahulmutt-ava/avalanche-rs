// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EVM + atomic-trie state sync over Firewood range/inclusion proofs (G8, spec
//! 10 §10 / §17.9; 04 §4.2/§4.3).
//!
//! These tests drive the leaf-request/response protocol against a real on-disk
//! Firewood revision (the `EvmStateSyncServer::handle_leafs` server side), the
//! client trie reconstruction + root verification, the EVM-state proof methods in
//! `state.rs` (`StateProofProvider`/`StorageRootProvider`), and the atomic-trie
//! sync + `ApplyToSharedMemory` flow over the second Firewood instance.
//!
//! # Wire-format provenance
//!
//! The leaf proof bytes are Firewood's own `FrozenRangeProof` binary
//! serialization (`firewood::api::FrozenRangeProof::write_to_vec`), which is
//! byte-identical to the Go `firewood/syncer` path: the Go FFI
//! `(*ffi.RangeProof).MarshalBinary()` calls `fwd_range_proof_to_bytes`, a thin
//! wrapper over the SAME firewood Rust serializer. The request/response envelope
//! mirrors `proto/sync/sync.proto`
//! (`RangeProofRequest{root_hash,start_key,end_key,key_limit,bytes_limit}` /
//! `ProofResponse{range_proof: bytes}`). See `tests/vectors/cchain/state_sync/`.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_database::BatchOps;
use ava_evm::atomic::trie::{AtomicTrie, trie_key};
use ava_evm::state::{
    FirewoodStateProvider, account_key, hashed_post_state_to_batchops, storage_key,
};
use ava_evm::sync::client::StateSyncClient;
use ava_evm::sync::server::{EvmStateSyncServer, LeafServer, RangeProofSource};
use ava_evm::sync::{LeafsRequest, MaybeBytes, apply_atomic_trie_to_shared_memory};
use ava_evm_reth::{
    Account, Address, B256, B256Map, HashedPostState, HashedStorage, StateProofProvider,
    StorageRootProvider, TrieInput, U256, keccak256,
};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};
use parking_lot::Mutex;

/// A minimal in-memory `SharedMemory` that records every `apply` call so the
/// atomic-sync test can assert `ApplyToSharedMemory` ran with the synced ops.
#[derive(Default)]
struct RecordingSharedMemory {
    applied: Mutex<Vec<(Id, Requests)>>,
}

impl SharedMemory for RecordingSharedMemory {
    fn get(&self, _peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, ava_vm::error::Error> {
        Ok(keys.iter().map(|_| Vec::new()).collect())
    }

    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> Result<IndexedResult, ava_vm::error::Error> {
        Ok((Vec::new(), Vec::new(), Vec::new()))
    }

    fn apply(
        &self,
        requests: BTreeMap<Id, Requests>,
        _batches: &[BatchOps],
    ) -> Result<(), ava_vm::error::Error> {
        let mut applied = self.applied.lock();
        for (chain, reqs) in requests {
            applied.push((chain, reqs));
        }
        Ok(())
    }
}

fn side_stores() -> (
    Arc<dyn ava_database::DynDatabase>,
    Arc<dyn ava_database::DynDatabase>,
) {
    (
        Arc::new(ava_database::MemDb::new()),
        Arc::new(ava_database::MemDb::new()),
    )
}

fn open_provider(dir: &std::path::Path) -> Arc<FirewoodStateProvider> {
    let (bytecode, block_hashes) = side_stores();
    FirewoodStateProvider::open(dir, bytecode, block_hashes).expect("open provider")
}

/// Commits `n` distinct accounts (no storage) into the provider, returning the
/// committed state root and the (address, account) set actually written.
fn seed_accounts(provider: &Arc<FirewoodStateProvider>, n: u8) -> (B256, Vec<(Address, Account)>) {
    let mut accounts = B256Map::default();
    let mut written = Vec::new();
    for i in 1..=n {
        let addr = Address::repeat_byte(i);
        let acct = Account {
            nonce: u64::from(i),
            balance: U256::from(1_000u64 * u64::from(i)),
            bytecode_hash: None,
        };
        accounts.insert(keccak256(addr), Some(acct));
        written.push((addr, acct));
    }
    let hashed = HashedPostState {
        accounts,
        storages: B256Map::default(),
    };
    let ops = hashed_post_state_to_batchops(&hashed);
    let root = provider.propose_and_stash(ops).expect("stash");
    provider.commit(root).expect("commit");
    (root, written)
}

/// G8 — the leaf server answers a range request from a Firewood historical
/// revision with a wire-exact (`FrozenRangeProof::write_to_vec`) range proof.
#[test]
fn leafs_request_served_from_firewood_revision() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = open_provider(dir.path());
    let (root, written) = seed_accounts(&provider, 6);

    // Commit a SECOND revision on top so the served root is genuinely historical.
    let (root2, _) = seed_accounts(&provider, 8);
    assert_ne!(root, root2, "second commit must advance the tip");

    let server = EvmStateSyncServer::new(Arc::clone(&provider));

    // Full-range request at the historical root.
    let req = LeafsRequest {
        root,
        start: MaybeBytes::nothing(),
        end: MaybeBytes::nothing(),
        key_limit: 2048,
        bytes_limit: 1 << 20,
    };
    let resp = server.handle_leafs(&req).expect("handle leafs");

    // The response carries the firewood range-proof bytes + the decoded keys.
    assert!(!resp.proof.is_empty(), "proof bytes must be present");
    // At the historical root we wrote exactly `written.len()` accounts.
    assert_eq!(resp.keys.len(), written.len());
    assert_eq!(resp.keys.len(), resp.vals.len());

    // Keys are the keccak(addr) account-trie keys, ascending.
    let mut want_keys: Vec<[u8; 32]> = written.iter().map(|(a, _)| account_key(a)).collect();
    want_keys.sort_unstable();
    let got_keys: Vec<[u8; 32]> = resp
        .keys
        .iter()
        .map(|k| <[u8; 32]>::try_from(k.as_slice()).expect("32-byte key"))
        .collect();
    assert_eq!(got_keys, want_keys);

    // The proof bytes round-trip through firewood's own deserializer and verify
    // against the served root (wire-format byte-exactness with Go syncer).
    EvmStateSyncServer::verify_proof_bytes(&resp.proof, root, None, None)
        .expect("served proof must verify against the historical root");

    // A request at a root outside the retained revision window is an error.
    let bogus = LeafsRequest {
        root: B256::repeat_byte(0xee),
        start: MaybeBytes::nothing(),
        end: MaybeBytes::nothing(),
        key_limit: 2048,
        bytes_limit: 1 << 20,
    };
    assert!(server.handle_leafs(&bogus).is_err());
}

/// G8 — the client reconstructs a Firewood trie from the served leaves and
/// verifies its root equals the target root (the syncer client contract).
#[test]
fn client_reconstructs_trie_and_verifies_root() {
    let server_dir = tempfile::tempdir().expect("server dir");
    let provider = open_provider(server_dir.path());
    let (root, written) = seed_accounts(&provider, 10);

    let server = EvmStateSyncServer::new(Arc::clone(&provider));

    // Drive the client: it requests leaf ranges, verifies each proof, and rebuilds
    // a fresh Firewood trie, finally checking the reconstructed root == target.
    let client_dir = tempfile::tempdir().expect("client dir");
    let mut client = StateSyncClient::open(client_dir.path(), root).expect("open client");
    client.sync_from(&server).expect("sync");
    assert_eq!(client.root(), root, "reconstructed root must equal target");

    // The reconstructed trie reads back every account the server held.
    let view = client.view().expect("client view");
    for (addr, acct) in &written {
        let got = view
            .basic_account(addr)
            .expect("read account")
            .expect("present");
        assert_eq!(&got, acct);
    }

    // A client opened against the WRONG target root rejects the reconstruction.
    let bad_dir = tempfile::tempdir().expect("bad dir");
    let mut bad = StateSyncClient::open(bad_dir.path(), B256::repeat_byte(0x01)).expect("open");
    assert!(bad.sync_from(&server).is_err());
}

/// G8 — the EVM-state proof methods (`state.rs`) read the firewood revision and
/// return verifiable inclusion proofs for accounts and storage slots
/// (`eth_getProof` seam, consumed by M6.23).
#[test]
fn state_proof_methods_serve_account_and_storage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let provider = open_provider(dir.path());

    let addr = Address::repeat_byte(0x55);
    let slot = B256::repeat_byte(0x66);
    let slot_value = U256::from(0xfeed_u64);

    let mut storages = B256Map::default();
    let mut slots = B256Map::default();
    slots.insert(keccak256(slot), slot_value);
    storages.insert(
        keccak256(addr),
        HashedStorage {
            wiped: false,
            storage: slots,
        },
    );
    let mut accounts = B256Map::default();
    accounts.insert(
        keccak256(addr),
        Some(Account {
            nonce: 3,
            balance: U256::from(7_777u64),
            bytecode_hash: None,
        }),
    );
    let hashed = HashedPostState { accounts, storages };
    let ops = hashed_post_state_to_batchops(&hashed);
    let root = provider.propose_and_stash(ops).expect("stash");
    provider.commit(root).expect("commit");

    let view = provider.view_tip().expect("view");

    // Sanity: the storage key helper is part of the proof seam.
    let _ = storage_key(&addr, &slot);

    // storage_proof: returns the slot value + a non-empty firewood inclusion proof.
    let sp = view
        .storage_proof(addr, slot, HashedStorage::default())
        .expect("storage proof");
    assert_eq!(sp.key, slot);
    assert_eq!(sp.value, slot_value);
    assert!(
        !sp.proof.is_empty(),
        "storage inclusion proof must be present"
    );

    // The account inclusion proof carries the read-back account info.
    let ap = view
        .proof(TrieInput::default(), addr, &[slot])
        .expect("account proof");
    assert_eq!(ap.address, addr);
    let info = ap.info.expect("account info present");
    assert_eq!(info.nonce, 3);
    assert_eq!(info.balance, U256::from(7_777u64));
    assert!(
        !ap.proof.is_empty(),
        "account inclusion proof must be present"
    );
    assert_eq!(ap.storage_proofs.len(), 1);
    assert_eq!(ap.storage_proofs[0].value, slot_value);

    // An absent account yields info == None.
    let absent = view
        .proof(TrieInput::default(), Address::repeat_byte(0x99), &[])
        .expect("absent proof");
    assert_eq!(absent.info, None);

    // storage_root reads the account leaf's encoded storage_root field. Firewood
    // v0.5 derives the live sub-trie root internally and does NOT expose it, so
    // the leaf carries the empty-trie sentinel (documented M6.25 limitation,
    // spec 10 §17.9); a present account therefore reports EMPTY_ROOT_HASH here.
    let sroot = view
        .storage_root(addr, HashedStorage::default())
        .expect("storage root");
    assert_eq!(sroot, ava_evm_reth::EMPTY_ROOT_HASH);
    // An absent account also reports the empty-trie root.
    let sroot_absent = view
        .storage_root(Address::repeat_byte(0x99), HashedStorage::default())
        .expect("absent storage root");
    assert_eq!(sroot_absent, ava_evm_reth::EMPTY_ROOT_HASH);
}

/// G8 — the atomic trie syncs over the SECOND Firewood instance the same way,
/// then `ApplyToSharedMemory` replays the synced cursor's ops into shared memory.
#[test]
fn atomic_trie_syncs_then_applies_to_shared_memory() {
    // Build a source atomic trie with two accepted blocks of requests.
    let src_dir = tempfile::tempdir().expect("src atomic dir");
    let src_trie = AtomicTrie::open(src_dir.path()).expect("open src trie");

    let chain_a = Id::from([0xaa; 32]);
    let chain_b = Id::from([0xbb; 32]);

    let mut blk1: BTreeMap<Id, Requests> = BTreeMap::new();
    blk1.insert(
        chain_a,
        Requests {
            remove: vec![vec![0x01; 32]],
            put: Vec::new(),
        },
    );
    let ops1 = AtomicTrie::ops_for_block(1, &blk1).expect("ops1");
    src_trie.commit(ops1).expect("commit blk1");

    let mut blk2: BTreeMap<Id, Requests> = BTreeMap::new();
    blk2.insert(
        chain_b,
        Requests {
            remove: vec![vec![0x02; 32]],
            put: Vec::new(),
        },
    );
    let ops2 = AtomicTrie::ops_for_block(2, &blk2).expect("ops2");
    let src_root = src_trie.commit(ops2).expect("commit blk2");
    // Release the exclusive writer handle so the server can open its own
    // read-only Firewood handle over the same data directory.
    drop(src_trie);

    // Sync the atomic trie into a fresh Firewood instance over range proofs.
    let dst_dir = tempfile::tempdir().expect("dst atomic dir");
    let mut atomic_client =
        StateSyncClient::open(dst_dir.path(), src_root).expect("open atomic client");
    let atomic_server = RangeProofSource::open(src_dir.path()).expect("open atomic source");
    atomic_client
        .sync_from(&atomic_server)
        .expect("atomic sync");
    assert_eq!(atomic_client.root(), src_root);

    // The synced trie has both block keys.
    let view = atomic_client.view().expect("view");
    assert!(view.raw_get(&trie_key(1, &chain_a)).expect("k1").is_some());
    assert!(view.raw_get(&trie_key(2, &chain_b)).expect("k2").is_some());

    // ApplyToSharedMemory: replay the synced cursor's per-block ops into shared
    // memory, starting from cursor height 0.
    let shared = Arc::new(RecordingSharedMemory::default());
    let cursor =
        apply_atomic_trie_to_shared_memory(&atomic_client, shared.as_ref(), 0).expect("apply");

    // The reconcile applied both chains' requests.
    let applied = shared.applied.lock();
    let chains: Vec<Id> = applied.iter().map(|(c, _)| *c).collect();
    assert!(chains.contains(&chain_a));
    assert!(chains.contains(&chain_b));
    // The cursor advanced to the highest synced height.
    assert_eq!(cursor, 2);
}

/// G8 — the served leaf bytes for a fixed single-account state reproduce the
/// committed golden vector (wire-format byte-exactness with the Go
/// `firewood/syncer`; see `tests/vectors/cchain/state_sync/_provenance.md`). A
/// firewood bump that changes the proof wire format trips this test.
#[test]
fn account_leaf_range_proof_golden_vector() {
    let raw = include_str!("vectors/cchain/state_sync/account_leaf_range_proof.json");
    let v: serde_json::Value = serde_json::from_str(raw).expect("json");

    let dir = tempfile::tempdir().expect("tempdir");
    let provider = open_provider(dir.path());
    let addr = Address::repeat_byte(0x01);
    let mut accounts = B256Map::default();
    accounts.insert(
        keccak256(addr),
        Some(Account {
            nonce: 1,
            balance: U256::from(1000u64),
            bytecode_hash: None,
        }),
    );
    let hashed = HashedPostState {
        accounts,
        storages: B256Map::default(),
    };
    let ops = hashed_post_state_to_batchops(&hashed);
    let root = provider.propose_and_stash(ops).expect("stash");
    provider.commit(root).expect("commit");

    // The committed root matches the vector.
    let want_root = v["state_root"].as_str().expect("state_root");
    assert_eq!(format!("{root}"), want_root);

    let server = EvmStateSyncServer::new(Arc::clone(&provider));
    let req = LeafsRequest {
        root,
        start: MaybeBytes::nothing(),
        end: MaybeBytes::nothing(),
        key_limit: 2048,
        bytes_limit: 1 << 20,
    };
    let resp = server.handle_leafs(&req).expect("leafs");

    assert_eq!(resp.keys.len(), 1);
    assert_eq!(
        hex::encode(&resp.keys[0]),
        v["leaf_key"].as_str().expect("leaf_key")
    );
    assert_eq!(
        hex::encode(&resp.vals[0]),
        v["leaf_value_rlp"].as_str().expect("leaf_value_rlp")
    );
    // The byte-exact wire proof.
    assert_eq!(
        hex::encode(&resp.proof),
        v["range_proof"].as_str().expect("range_proof")
    );

    // And it verifies against the served root.
    EvmStateSyncServer::verify_proof_bytes(&resp.proof, root, None, None).expect("verify");
}
