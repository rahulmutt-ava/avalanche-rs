// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Mempool + block builder conformance tests (M5.17, specs 09 §7.1).
//!
//! Tests:
//! - Happy-path: N valid txs in the mempool → `build_block` drains them in FIFO
//!   order, packs into `StandardBlock`, verifies height/time clamping.
//! - Conflict/invalid tx dropped while the rest pack.
//! - `time = max(parent_time, now)` clamping.
//! - Byte-cap packing: stops once cumulative tx bytes exceed `TARGET_BLOCK_SIZE`.
//! - `mempool_pop_order_total` proptest: FIFO order is a stable total order.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use proptest::prelude::*;

use ava_avm::block::builder::{BuildBlockOutput, BuildBlockParams, TARGET_BLOCK_SIZE, build_block};
use ava_avm::block::executor::{BlockManager, BlockManagerConfig};
use ava_avm::fx::dispatch::Dispatch;
use ava_avm::mempool::Mempool;
use ava_avm::state::{Chain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo;
use ava_avm::txs::executor::{Backend, Config};
use ava_avm::txs::{BaseTx, CreateAssetTx, FxCredential, InitialState, Tx, UnsignedTx};
use ava_database::MemDb;
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};

const NETWORK_ID: u32 = 10;
const TX_FEE: u64 = 0;
const CREATE_ASSET_TX_FEE: u64 = 0;
const NUM_FXS: usize = 3;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

fn backend() -> Backend {
    Backend::new(
        NETWORK_ID,
        chain_id(),
        Config::new(TX_FEE, CREATE_ASSET_TX_FEE),
        Id::EMPTY,
        NUM_FXS,
        false,
    )
}

fn dispatch() -> Dispatch {
    Dispatch::new(
        Id::EMPTY,
        Id::from([1u8; 32]),
        Id::from([2u8; 32]),
        Arc::new(MockClock::default()),
    )
}

fn secp_credential() -> FxCredential {
    FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
}

fn transfer_output(asset_id: Id, amt: u64) -> TransferableOutput {
    TransferableOutput {
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    }
}

fn transfer_input(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> TransferableInput {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    TransferableInput {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id,
        r#in: Input::SecpTransfer(TransferInput::new(amt, vec![0])),
    }
}

fn utxo_bytes_pair(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    let utxo = Utxo {
        tx_id: Id::from(tx_id),
        output_index: idx,
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(amt, owners())),
    };
    (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
}

fn signed(unsigned: UnsignedTx, num_creds: usize) -> Tx {
    let mut tx = Tx::new(unsigned);
    tx.creds = (0..num_creds).map(|_| secp_credential()).collect();
    tx.initialize(Codec()).expect("initialize tx");
    tx
}

fn create_asset_tx() -> Tx {
    let ca = CreateAssetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: Vec::new(),
            ins: Vec::new(),
            memo: Vec::new(),
        }),
        name: "Asset".to_string(),
        symbol: "MYA".to_string(),
        denomination: 8,
        states: vec![InitialState::new(
            0,
            vec![Output::SecpTransfer(TransferOutput::new(1, owners()))],
        )],
    };
    let mut tx = Tx::new(UnsignedTx::CreateAsset(ca));
    tx.initialize(Codec()).expect("initialize create-asset");
    tx
}

/// No-op shared memory for builder tests (no import/export txs).
struct NopSharedMemory;

impl SharedMemory for NopSharedMemory {
    fn get(
        &self,
        _peer_chain: Id,
        _keys: &[Vec<u8>],
    ) -> Result<Vec<Vec<u8>>, ava_vm::error::Error> {
        Ok(Vec::new())
    }

    fn indexed(
        &self,
        _peer_chain: Id,
        _traits: &[Vec<u8>],
        _start_trait: &[u8],
        _start_key: &[u8],
        _limit: usize,
    ) -> Result<IndexedResult, ava_vm::error::Error> {
        unimplemented!()
    }

    fn apply(
        &self,
        _requests: BTreeMap<Id, Requests>,
        _batches: &[ava_database::BatchOps],
    ) -> Result<(), ava_vm::error::Error> {
        Ok(())
    }
}

/// Builds a fresh genesis-seeded `State<MemDb>` with the given seed.
/// Returns the `BlockManager` (for accept/verify) and a snapshot `Arc<dyn Chain>`
/// to pass to `build_block`.
fn genesis_setup(
    genesis_blk_id: Id,
    seed: impl FnOnce(&mut State<MemDb>),
) -> (BlockManager<MemDb>, Arc<dyn Chain>) {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");
    state.set_last_accepted(genesis_blk_id);
    seed(&mut state);
    state.commit().expect("commit genesis");

    let sm = Arc::new(NopSharedMemory);
    let cfg = BlockManagerConfig {
        backend: backend(),
        dispatch: dispatch(),
        shared_memory: Arc::clone(&sm) as Arc<dyn SharedMemory>,
    };
    let mgr = BlockManager::new(state, cfg);
    let snapshot = mgr.state().snapshot();
    (mgr, snapshot)
}

// ---------------------------------------------------------------------------
// Happy path: N valid txs → drained in FIFO order into StandardBlock
// ---------------------------------------------------------------------------

#[test]
fn build_block_happy_path_n_txs() {
    let genesis_id = Id::from([0x01; 32]);
    let ca = create_asset_tx();
    let asset_id = ca.id();

    // Seed three UTXOs to spend.
    let (utxo_id_a, utxo_a) = utxo_bytes_pair(0x10, 0, asset_id, 1000);
    let (utxo_id_b, utxo_b) = utxo_bytes_pair(0x20, 0, asset_id, 1000);
    let (utxo_id_c, utxo_c) = utxo_bytes_pair(0x30, 0, asset_id, 1000);

    let (mut mgr, parent_state) = genesis_setup(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id_a, utxo_a);
        s.add_utxo(utxo_id_b, utxo_b);
        s.add_utxo(utxo_id_c, utxo_c);
    });

    // Build three txs spending distinct UTXOs.
    let tx_a = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x10, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );
    let tx_b = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x20, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );
    let tx_c = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x30, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );

    let id_a = tx_a.id();
    let id_b = tx_b.id();
    let id_c = tx_c.id();

    let mut mempool = Mempool::new();
    mempool.add(tx_a).expect("add tx_a");
    mempool.add(tx_b).expect("add tx_b");
    mempool.add(tx_c).expect("add tx_c");

    let candidate_txs = mempool.snapshot();

    let c = ava_avm::txs::codec::codec().expect("codec");
    let parent_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1_000_000);

    let BuildBlockOutput {
        block: blk,
        dropped,
    } = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now,
        parent_state,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs,
    })
    .expect("build_block");

    // Block should contain all three txs in FIFO order.
    assert_eq!(blk.txs().len(), 3, "all three txs packed");
    assert_eq!(blk.txs()[0].id(), id_a);
    assert_eq!(blk.txs()[1].id(), id_b);
    assert_eq!(blk.txs()[2].id(), id_c);

    // No txs should be dropped in the happy path.
    assert!(dropped.is_empty(), "no drops expected; got: {dropped:?}");

    // Height = parent_height + 1 = 1.
    assert_eq!(blk.height(), 1);

    // time = max(parent_time, now) = now (in seconds).
    let expected_time = 1_000_000u64;
    assert_eq!(blk.timestamp(), expected_time);

    // parent_id matches.
    assert_eq!(blk.parent_id(), genesis_id);

    // Verify the block via the manager.
    mgr.verify(&blk).expect("verify should pass");
}

// ---------------------------------------------------------------------------
// Conflicting / invalid tx dropped; rest still pack
// ---------------------------------------------------------------------------

#[test]
fn build_block_drops_conflicting_tx() {
    let genesis_id = Id::from([0x02; 32]);
    let ca = create_asset_tx();
    let asset_id = ca.id();

    let (utxo_id_a, utxo_a) = utxo_bytes_pair(0x11, 0, asset_id, 1000);
    let (utxo_id_b, utxo_b) = utxo_bytes_pair(0x22, 0, asset_id, 1000);

    let (_, parent_state) = genesis_setup(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id_a, utxo_a);
        s.add_utxo(utxo_id_b, utxo_b);
    });

    let tx_ok = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x11, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );
    // tx_conflict spends the same UTXO as tx_ok — should be dropped.
    let tx_conflict = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x11, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );
    let tx_ok2 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x22, 0, asset_id, 1000)],
            memo: Vec::new(),
        })),
        1,
    );

    let id_ok = tx_ok.id();
    let id_conflict = tx_conflict.id();
    let id_ok2 = tx_ok2.id();

    let c = ava_avm::txs::codec::codec().expect("codec");
    // Pass candidate txs in order: ok, conflict, ok2.
    let candidate_txs = vec![tx_ok, tx_conflict, tx_ok2];
    let parent_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(2_000_000);

    let BuildBlockOutput {
        block: blk,
        dropped,
    } = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now,
        parent_state,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs,
    })
    .expect("build_block");

    // Only the two non-conflicting txs should be packed.
    assert_eq!(blk.txs().len(), 2, "conflicting tx dropped");
    assert_eq!(blk.txs()[0].id(), id_ok);
    assert_eq!(blk.txs()[1].id(), id_ok2);

    // The conflicting tx must be recorded in the dropped list.
    assert_eq!(dropped.len(), 1, "exactly one tx should be dropped");
    assert_eq!(
        dropped[0].0, id_conflict,
        "the dropped tx id must be tx_conflict"
    );
}

// ---------------------------------------------------------------------------
// time = max(parent_time, now) clamping
// ---------------------------------------------------------------------------

#[test]
fn build_block_time_clamp_uses_max_of_parent_time_and_now() {
    let genesis_id = Id::from([0x03; 32]);
    let ca = create_asset_tx();
    let asset_id = ca.id();

    let (utxo_id, utxo) = utxo_bytes_pair(0x33, 0, asset_id, 500);

    let tx = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 500)],
            ins: vec![transfer_input(0x33, 0, asset_id, 500)],
            memo: Vec::new(),
        })),
        1,
    );

    let c = ava_avm::txs::codec::codec().expect("codec");

    // Case 1: now < parent_time → time = parent_time.
    let parent_time_secs = 5_000_000u64;
    let parent_time = UNIX_EPOCH + Duration::from_secs(parent_time_secs);
    let now_earlier = UNIX_EPOCH + Duration::from_secs(1_000);

    let (_, parent_state1) = genesis_setup(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo.clone());
    });

    let BuildBlockOutput { block: blk1, .. } = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now: now_earlier,
        parent_state: parent_state1,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs: vec![tx.clone()],
    })
    .expect("build_block case1");
    assert_eq!(
        blk1.timestamp(),
        parent_time_secs,
        "time should be clamped to parent_time when now < parent_time"
    );

    // Case 2: now > parent_time → time = now.
    let now_later = UNIX_EPOCH + Duration::from_secs(10_000_000);

    let (_, parent_state2) = genesis_setup(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo);
    });

    let BuildBlockOutput { block: blk2, .. } = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now: now_later,
        parent_state: parent_state2,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs: vec![tx],
    })
    .expect("build_block case2");
    assert_eq!(
        blk2.timestamp(),
        10_000_000u64,
        "time should be now when now > parent_time"
    );
}

// ---------------------------------------------------------------------------
// Byte-cap packing: stops once cumulative tx bytes exceed TARGET_BLOCK_SIZE
// ---------------------------------------------------------------------------

#[test]
fn build_block_respects_byte_cap() {
    let genesis_id = Id::from([0x04; 32]);
    let ca = create_asset_tx();
    let asset_id = ca.id();

    // Use 200-byte memos (below the 256-byte syntactic limit) to make each tx
    // large enough that several of them exceed TARGET_BLOCK_SIZE (128 KiB).
    // With ~200-byte memos, each tx ≈ 500+ bytes serialized → ~256+ txs to fill
    // the cap. Use enough txs to guarantee the cap fires.
    let memo_size: usize = 200;
    // Each tx serializes to roughly memo_size + ~300 bytes overhead.
    // TARGET_BLOCK_SIZE (128 KiB) / ~500 bytes per tx ≈ 256 txs to fill the cap.
    // We use 512 UTXOs to ensure we exceed the cap.
    let n: usize = 512;

    // Seed UTXOs: use (byte0, byte1) pair to address up to 512 unique UTXOs.
    let mut utxo_ids: Vec<Id> = Vec::with_capacity(n);
    let mut utxo_store: Vec<(Id, Vec<u8>)> = Vec::with_capacity(n);
    for i in 0..n {
        let b0 = (i / 256) as u8;
        let b1 = (i % 256) as u8;
        // Construct a unique tx_id for the genesis UTXO.
        let mut raw = [0u8; 32];
        raw[0] = b0;
        raw[1] = b1;
        raw[2] = 0xAA;
        let tx_id = Id::from(raw);
        let utxo = Utxo {
            tx_id,
            output_index: 0,
            asset_id,
            out: Output::SecpTransfer(TransferOutput::new(1000, owners())),
        };
        let utxo_id = utxo.input_id();
        let utxo_bytes = utxo.marshal().expect("marshal utxo");
        utxo_ids.push(utxo_id);
        utxo_store.push((utxo_id, utxo_bytes));
    }

    let (_, parent_state) = genesis_setup(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        for (id, bytes) in &utxo_store {
            s.add_utxo(*id, bytes.clone());
        }
    });

    // Build a tx for each UTXO with a 200-byte memo (≤ 256 byte limit).
    let txs: Vec<Tx> = (0..n)
        .map(|i| {
            let b0 = (i / 256) as u8;
            let b1 = (i % 256) as u8;
            let mut raw = [0u8; 32];
            raw[0] = b0;
            raw[1] = b1;
            raw[2] = 0xAA;
            let tx_id_ref = Id::from(raw);
            let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
                network_id: NETWORK_ID,
                blockchain_id: chain_id(),
                outs: vec![transfer_output(asset_id, 1000)],
                ins: vec![TransferableInput {
                    tx_id: tx_id_ref,
                    output_index: 0,
                    asset_id,
                    r#in: Input::SecpTransfer(TransferInput::new(1000, vec![0])),
                }],
                memo: vec![0u8; memo_size],
            })));
            tx.creds = vec![secp_credential()];
            tx.initialize(Codec()).expect("initialize tx");
            tx
        })
        .collect();

    let tx_size = txs[0].size();
    // How many txs would fit under TARGET_BLOCK_SIZE?
    // The first is always packed; subsequent ones stop when cumulative > cap.
    let max_packed = TARGET_BLOCK_SIZE.saturating_div(tx_size).max(1);

    let c = ava_avm::txs::codec::codec().expect("codec");
    let parent_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1_000_000);

    let BuildBlockOutput { block: blk, .. } = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now,
        parent_state,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs: txs,
    })
    .expect("build_block");

    // Should have packed at most max_packed + 1 txs.
    assert!(
        blk.txs().len() <= max_packed + 1,
        "byte cap should limit packing; got {} txs, max_packed={max_packed}, tx_size={tx_size}",
        blk.txs().len()
    );
    assert!(
        blk.txs().len() < n,
        "should not have packed all {n} txs due to byte cap"
    );
}

// ---------------------------------------------------------------------------
// NoPendingBlocks: no txs → error
// ---------------------------------------------------------------------------

#[test]
fn build_block_no_txs_returns_no_pending_blocks() {
    use ava_avm::error::Error;

    let genesis_id = Id::from([0x05; 32]);
    let (_, parent_state) = genesis_setup(genesis_id, |_| {});

    let c = ava_avm::txs::codec::codec().expect("codec");
    let parent_time = UNIX_EPOCH;
    let now = UNIX_EPOCH + Duration::from_secs(1_000_000);

    let result = build_block(BuildBlockParams {
        codec: &c,
        parent_id: genesis_id,
        parent_height: 0,
        parent_time,
        now,
        parent_state,
        backend: &backend(),
        dispatch: &dispatch(),
        candidate_txs: vec![],
    });

    assert!(
        matches!(result, Err(Error::NoPendingBlocks)),
        "expected NoPendingBlocks, got: {result:?}"
    );
}

// ---------------------------------------------------------------------------
// mempool_pop_order_total: FIFO is a stable total order independent of map internals
//
// The key property (spec 00 §6.1): given the SAME insertion sequence, snapshot()
// is deterministic and equals the insertion sequence — across runs and independent
// of tx-id hash values.  A HashMap-backed impl that returned map-iteration order
// would fail because HashMap randomises iteration per run.
// ---------------------------------------------------------------------------

proptest! {
    /// Adds a proptest-shuffled sequence of distinct txs and asserts that
    /// `snapshot()` / `peek()` order equals EXACTLY the insertion order on every run.
    ///
    /// The txs have varied IDs (derived from proptest-controlled `perm` indices),
    /// so any implementation that uses hash-map iteration order for its output would
    /// produce a different, non-deterministic sequence — and fail this test.
    #[test]
    fn mempool_pop_order_total(
        // A permutation of [0..N): each element is a unique index in 0..64 that
        // determines both the memo bytes (→ distinct tx ID) and the insertion order.
        perm in proptest::collection::vec(0u32..64u32, 2..20usize)
    ) {
        // Deduplicate to get a unique insertion sequence.
        let mut seen = std::collections::HashSet::new();
        let insertion_order: Vec<u32> = perm.into_iter().filter(|t| seen.insert(*t)).collect();
        if insertion_order.len() < 2 {
            return Ok(());
        }

        let c = ava_avm::txs::codec::codec().expect("codec");

        // Build txs with varied IDs: memo = 4-byte big-endian of the index value,
        // mixed with a fixed high byte so ids spread widely across hash space.
        // This means a HashMap-backed implementation that uses hash-ordered
        // iteration would return them in a different order than insertion order.
        let txs: Vec<Tx> = insertion_order.iter().map(|&idx| {
            // Embed idx in the last two bytes of a 4-byte memo so IDs are
            // spread across the full 32-byte ID space (via SHA-256 of bytes).
            let mut memo = [0xDE_u8, 0xAD, 0u8, 0u8];
            memo[2] = (idx >> 8) as u8;
            memo[3] = idx as u8;
            let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![],
                memo: memo.to_vec(),
            })));
            tx.initialize(&c).expect("initialize tx");
            tx
        }).collect();

        // Verify all tx IDs are distinct (required for the FIFO test to be meaningful).
        let ids: Vec<Id> = txs.iter().map(Tx::id).collect();
        let id_set: std::collections::HashSet<_> = ids.iter().copied().collect();
        prop_assert_eq!(
            id_set.len(), txs.len(),
            "all tx IDs must be distinct so hash-ordering is a real threat"
        );

        // --- Insert in the proptest-controlled order ---
        let mut pool = Mempool::new();
        for tx in &txs {
            pool.add(tx.clone()).expect("add to pool");
        }

        // --- snapshot() must equal EXACTLY the insertion order ---
        let snap: Vec<Id> = pool.snapshot().iter().map(Tx::id).collect();
        prop_assert_eq!(
            &snap, &ids,
            "snapshot must equal the insertion sequence (FIFO); \
             a HashMap-backed impl would fail here because its iteration \
             order depends on hash values, not insertion order"
        );

        // --- peek() must be the FIRST inserted tx ---
        prop_assert_eq!(
            pool.peek().map(Tx::id),
            Some(ids[0]),
            "peek must return the oldest (first inserted) tx"
        );

        // --- After removing the front, peek advances to the second ---
        let front_id = ids[0];
        pool.remove(&front_id).expect("remove front");
        prop_assert_eq!(
            pool.snapshot().iter().map(Tx::id).collect::<Vec<_>>(),
            &ids[1..],
            "after removing front, snapshot must equal insertion order sans the first"
        );
        if ids.len() > 1 {
            prop_assert_eq!(
                pool.peek().map(Tx::id),
                Some(ids[1]),
                "peek after removing front must be the second inserted tx"
            );
        }
    }
}
