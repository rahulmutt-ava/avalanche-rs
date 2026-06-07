// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Block lifecycle conformance tests: verify / accept / reject + atomic commit
//! (M5.16, specs 09 §7 + §6, 07 §2.3 Block trait).
//!
//! Each test seeds a `State<MemDb>`, builds a `StandardBlock` through the
//! `BlockManager`, runs verify → accept (or reject), then re-reads persisted
//! state and asserts the exact postconditions Go's block executor enforces.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use assert_matches::assert_matches;
use ava_avm::block::executor::{BlockManager, BlockManagerConfig};
use ava_avm::block::standard_block::StandardBlock;
use ava_avm::fx::dispatch::Dispatch;
use ava_avm::state::{Chain, ReadOnlyChain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo;
use ava_avm::txs::executor::{Backend, Config};
use ava_avm::txs::{BaseTx, CreateAssetTx, ExportTx, FxCredential, InitialState, Tx, UnsignedTx};
use ava_database::MemDb;
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::components::avax::shared_memory::{IndexedResult, Requests, SharedMemory};

const NETWORK_ID: u32 = 10;
/// Zero fees simplify test setup: we don't need to carry a fee-asset UTXO
/// alongside every spending UTXO, since the fee check (`avax.FlowChecker`)
/// in the syntactic verifier would otherwise require `fee_asset_id` inputs.
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

fn secp_credential() -> FxCredential {
    FxCredential::new(Id::EMPTY, SecpCredential::new(vec![[0u8; 65]]))
}

fn utxo_bytes(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
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

/// A recording `SharedMemory` double for the atomic-apply test.
struct RecordingSharedMemory {
    applied: parking_lot::Mutex<Vec<BTreeMap<Id, Requests>>>,
}

impl RecordingSharedMemory {
    fn new() -> Self {
        Self {
            applied: parking_lot::Mutex::new(Vec::new()),
        }
    }

    fn take_applied(&self) -> Vec<BTreeMap<Id, Requests>> {
        std::mem::take(&mut *self.applied.lock())
    }
}

impl SharedMemory for RecordingSharedMemory {
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
        requests: BTreeMap<Id, Requests>,
        _batches: &[ava_database::BatchOps],
    ) -> Result<(), ava_vm::error::Error> {
        self.applied.lock().push(requests);
        Ok(())
    }
}

/// Builds a fresh genesis-seeded `State` + `BlockManager`. The genesis block id
/// is `genesis_blk_id`; the state's last-accepted is set to it.
fn genesis_manager(
    genesis_blk_id: Id,
    seed: impl FnOnce(&mut State<MemDb>),
) -> (BlockManager<MemDb>, Arc<RecordingSharedMemory>) {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");
    state.set_last_accepted(genesis_blk_id);
    seed(&mut state);
    state.commit().expect("commit genesis");

    let sm = Arc::new(RecordingSharedMemory::new());
    let cfg = BlockManagerConfig {
        backend: backend(),
        dispatch: dispatch(),
        shared_memory: Arc::clone(&sm) as Arc<dyn SharedMemory>,
    };
    let mgr = BlockManager::new(state, cfg);
    (mgr, sm)
}

// ---------------------------------------------------------------------------
// verify → accept: BaseTx — UTXO set changes, last-accepted advances
// ---------------------------------------------------------------------------

#[test]
fn accept_base_tx_updates_utxo_set_and_last_accepted() {
    let genesis_id = Id::from([0xaa; 32]);

    // Create the asset tx + a UTXO spending it.
    let ca = create_asset_tx();
    let asset_id = ca.id();

    let (utxo_id, utxo_bytes_data) = utxo_bytes(0xbb, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo_bytes_data);
    });

    // Build a BaseTx: consume the UTXO, produce a new output.
    let base_tx = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xbb, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let tx_id = base_tx.id();
    let tx_bytes = base_tx.bytes().to_vec();

    let c = ava_avm::txs::codec::codec().expect("codec");
    let blk =
        StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![base_tx]).expect("build block");
    let blk_id = blk.id();

    mgr.verify(&blk).expect("verify");
    mgr.accept(&blk).expect("accept");

    // Re-read persisted state.
    let s = mgr.state();

    // The input UTXO must be gone.
    assert!(
        s.get_utxo(utxo_id).is_err(),
        "input UTXO must be deleted after accept"
    );

    // The output UTXO must exist (index 0 of tx).
    let out_utxo = Utxo {
        tx_id,
        output_index: 0,
        asset_id,
        out: Output::SecpTransfer(TransferOutput::new(1000, owners())),
    };
    let out_utxo_id = out_utxo.input_id();
    assert!(
        s.get_utxo(out_utxo_id).is_ok(),
        "output UTXO must be added after accept"
    );

    // Tx bytes stored.
    assert_eq!(s.get_tx(tx_id).expect("get_tx"), tx_bytes);

    // Block bytes / height index stored.
    assert!(s.get_block(blk_id).is_ok(), "block bytes stored");
    assert_eq!(s.get_block_id_at_height(1), Some(blk_id));

    // last_accepted advanced to the new block.
    assert_eq!(s.get_last_accepted(), blk_id);
    assert_eq!(mgr.last_accepted(), blk_id);

    // Timestamp updated.
    assert_ne!(s.get_timestamp(), UNIX_EPOCH);
}

// ---------------------------------------------------------------------------
// verify → reject: persisted state unchanged, diff discarded
// ---------------------------------------------------------------------------

#[test]
fn reject_leaves_state_unchanged() {
    let genesis_id = Id::from([0xcc; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id, utxo_bytes_data) = utxo_bytes(0xdd, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo_bytes_data.clone());
    });

    let base_tx = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xdd, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );

    let c = ava_avm::txs::codec::codec().expect("codec");
    let blk =
        StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![base_tx]).expect("build block");

    mgr.verify(&blk).expect("verify");
    mgr.reject(&blk);

    let s = mgr.state();

    // UTXO still present — reject must not touch persisted state.
    assert!(
        s.get_utxo(utxo_id).is_ok(),
        "reject must not delete the input UTXO"
    );

    // last_accepted not advanced.
    assert_eq!(s.get_last_accepted(), genesis_id);
    assert_eq!(mgr.last_accepted(), genesis_id);

    // Diff cache cleared — a second reject is a no-op (no panic).
    mgr.reject(&blk);
}

// ---------------------------------------------------------------------------
// double-spend: semantic verify catches it — second tx fails
// ---------------------------------------------------------------------------

#[test]
fn double_spend_verify_returns_error() {
    let genesis_id = Id::from([0xee; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id, utxo_bytes_data) = utxo_bytes(0xff, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo_bytes_data);
    });

    // Two txs both spending the SAME utxo.
    let tx1 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xff, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let tx2 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xff, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );

    let c = ava_avm::txs::codec::codec().expect("codec");
    let blk = StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![tx1, tx2])
        .expect("build block");

    // verify must fail with a Database NotFound: the second tx's semantic
    // verifier calls diff.get_utxo(...) for the doubly-spent UTXO, which the
    // first tx's executor already tombstoned in the shared diff.
    assert_matches!(
        mgr.verify(&blk),
        Err(ava_avm::error::Error::Database(
            ava_database::error::Error::NotFound
        )),
        "double-spend must surface as NotFound on the second tx's semantic verify"
    );
}

// ---------------------------------------------------------------------------
// ExportTx: atomic apply goes through SharedMemory in the same batch
// ---------------------------------------------------------------------------

#[test]
fn accept_export_tx_applies_atomic_requests() {
    let genesis_id = Id::from([0x11; 32]);
    let dest_chain = Id::from([0x22; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id, utxo_bytes_data) = utxo_bytes(0x33, 0, asset_id, 3000);

    let (mut mgr, sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo_bytes_data);
    });

    let export_tx = signed(
        UnsignedTx::Export(ExportTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: NETWORK_ID,
                blockchain_id: chain_id(),
                outs: vec![transfer_output(asset_id, 1000)],
                ins: vec![transfer_input(0x33, 0, asset_id, 3000)],
                memo: Vec::new(),
            }),
            destination_chain: dest_chain,
            exported_outs: vec![transfer_output(asset_id, 1000)],
        }),
        1,
    );

    let c = ava_avm::txs::codec::codec().expect("codec");
    let blk = StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![export_tx])
        .expect("build block");

    mgr.verify(&blk).expect("verify export");
    mgr.accept(&blk).expect("accept export");

    // SharedMemory::apply must have been called exactly once with a non-empty
    // requests map keyed by dest_chain.
    let calls = sm.take_applied();
    assert_eq!(calls.len(), 1, "SharedMemory::apply must be called once");
    let reqs = &calls[0];
    assert!(
        reqs.contains_key(&dest_chain),
        "atomic requests must be keyed by destination chain"
    );
    let chain_reqs = &reqs[&dest_chain];
    assert!(
        !chain_reqs.put.is_empty(),
        "export must produce at least one put request"
    );
}

// ---------------------------------------------------------------------------
// Verify a second block stacked on top of an unaccepted first block (chain of
// processing blocks resolved through BlockManager::get_state).
// ---------------------------------------------------------------------------

#[test]
fn verify_chain_of_two_processing_blocks() {
    let genesis_id = Id::from([0x44; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id1, utxo_bytes1) = utxo_bytes(0x55, 0, asset_id, 2000);
    let (utxo_id2, utxo_bytes2) = utxo_bytes(0x66, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id1, utxo_bytes1);
        s.add_utxo(utxo_id2, utxo_bytes2);
    });

    let c = ava_avm::txs::codec::codec().expect("codec");

    // Block 1 spends utxo_id1.
    let tx1 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x55, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let blk1 =
        StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![tx1]).expect("build blk1");
    let blk1_id = blk1.id();

    mgr.verify(&blk1).expect("verify blk1");

    // Block 2 (child of blk1) spends utxo_id2. blk1 is not yet accepted but
    // its diff is cached; blk2's verify must resolve blk1's diff as parent.
    let tx2 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x66, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let blk2 = StandardBlock::new_block(&c, blk1_id, 2, 1_000_001, vec![tx2]).expect("build blk2");

    mgr.verify(&blk2)
        .expect("verify blk2 on top of unaccepted blk1");
}

// ---------------------------------------------------------------------------
// Accept two sequential blocks: each flush advances state correctly
// ---------------------------------------------------------------------------

#[test]
fn accept_two_sequential_blocks() {
    let genesis_id = Id::from([0x77; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id1, utxo_bytes1) = utxo_bytes(0x88, 0, asset_id, 2000);
    let (utxo_id2, utxo_bytes2) = utxo_bytes(0x99, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id1, utxo_bytes1);
        s.add_utxo(utxo_id2, utxo_bytes2);
    });

    let c = ava_avm::txs::codec::codec().expect("codec");

    let tx1 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x88, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let blk1 = StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![tx1]).expect("blk1");
    let blk1_id = blk1.id();

    let tx2 = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0x99, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let blk2 = StandardBlock::new_block(&c, blk1_id, 2, 1_000_001, vec![tx2]).expect("blk2");
    let blk2_id = blk2.id();

    mgr.verify(&blk1).expect("verify blk1");
    mgr.verify(&blk2).expect("verify blk2");
    mgr.accept(&blk1).expect("accept blk1");

    // After accepting blk1, blk2's parent (blk1) is now last-accepted.
    assert_eq!(mgr.last_accepted(), blk1_id);
    assert!(mgr.state().get_utxo(utxo_id1).is_err(), "utxo1 gone");
    assert!(mgr.state().get_utxo(utxo_id2).is_ok(), "utxo2 still there");

    mgr.accept(&blk2).expect("accept blk2");
    assert_eq!(mgr.last_accepted(), blk2_id);
    assert!(mgr.state().get_utxo(utxo_id2).is_err(), "utxo2 gone");
}

// ---------------------------------------------------------------------------
// A second BaseTx block (asset reused from a seeded CreateAssetTx) — smoke test
// ---------------------------------------------------------------------------

#[test]
fn accept_base_tx_reusing_seeded_asset_smoke() {
    let genesis_id = Id::from([0xa0; 32]);

    let ca = create_asset_tx();
    let asset_id = ca.id();
    let (utxo_id, utxo_bytes_data) = utxo_bytes(0xa1, 0, asset_id, 2000);

    let (mut mgr, _sm) = genesis_manager(genesis_id, |s| {
        s.add_tx(asset_id, ca.bytes().to_vec());
        s.add_utxo(utxo_id, utxo_bytes_data);
    });

    let c = ava_avm::txs::codec::codec().expect("codec");

    let tx = signed(
        UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: chain_id(),
            outs: vec![transfer_output(asset_id, 1000)],
            ins: vec![transfer_input(0xa1, 0, asset_id, 2000)],
            memo: Vec::new(),
        })),
        1,
    );
    let blk =
        StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![tx]).expect("build block");

    mgr.verify(&blk).expect("verify block");
    mgr.accept(&blk).expect("accept block");
    assert_eq!(mgr.last_accepted(), blk.id());
}
