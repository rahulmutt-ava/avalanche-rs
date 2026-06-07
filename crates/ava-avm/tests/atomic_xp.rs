// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.20 — X↔P atomic import/export end-to-end (ATOMIC-1, specs 09 §9, 07
//! ATOMIC-1, 00 §11.1.7) exercised through the REAL cross-chain shared-memory
//! backend (`ava-chains`).
//!
//! These tests prove the **byte contract** of cross-chain UTXO transfer: a UTXO
//! the X-Chain exports is byte-identically decodable by the P-Chain (and the
//! reverse), and the bytes survive a full round-trip through the real
//! `ava-chains` shared-memory channel (sharedID prefixing + `dbElement`
//! encoding), not just `marshal`/`unmarshal` in isolation.
//!
//! Go references:
//! - `vms/components/avax/utxo.go` — the `avax.UTXO` wire layout.
//! - `vms/avm/txs/executor/executor.go` `ExportTx` — `Element.Key =
//!   utxo.InputID()[:]`, `Element.Value = Codec.Marshal(CodecVersion, utxo)`,
//!   `Element.Traits = out.Addresses()`.
//! - `chains/atomic` — the shared-memory channel + `dbElement` framing.
//! - `vms/secp256k1fx` — `TransferOutput` registered at type_id **7** on BOTH
//!   the avm (`vms/avm/txs/codec.go`) and the platformvm
//!   (`vms/platformvm/txs/codec.go`); that shared type-id is what makes the
//!   cross-decode byte-exact. Both sides use `CodecVersion = 0`.
//!
//! ## Recorded-oracle mode (per-PR gate)
//!
//! The differential harness (`tests/differential/`) is a tier-X scaffold: its
//! `LockstepDriver::replay_recorded` is unimplemented (owned by task X.13) and
//! there is no live two-binary mode. So `differential::atomic_xp` is delivered
//! here as a **recorded / self-consistent + Go-vector test** using the real
//! `ava-chains` shared-memory backend + cross-crate decode — matching the
//! M5.5/M5.15 self-consistent-golden precedent. The live two-binary
//! `differential::atomic_xp` is gated behind the unfinished harness (spec 09 §9
//! deferral list); the byte contract proven here is the per-PR gate.
//!
//! ## Deferral: VM-`initialize` production wiring
//!
//! Wiring cross-chain shared memory into `Vm::initialize` is DEFERRED:
//! `ChainContext` has no `shared_memory` field, so the real backend is supplied
//! by the chain manager (M8 / chain-manager follow-up). M5.20 proves the byte
//! contract + accept-path co-commit at the `BlockManager` level — which already
//! accepts an `Arc<dyn SharedMemory>` — with the real `ava-chains` backend, not
//! by changing the `Vm::initialize` signature.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_avm::block::executor::{BlockManager, BlockManagerConfig};
use ava_avm::block::standard_block::StandardBlock;
use ava_avm::fx::dispatch::Dispatch;
use ava_avm::state::{Chain, ReadOnlyChain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo as AvmUtxo;
use ava_avm::txs::executor::{Backend, Config};
use ava_avm::txs::{BaseTx, CreateAssetTx, ExportTx, FxCredential, InitialState, Tx, UnsignedTx};
use ava_chains::atomic::shared_memory::Memory;
use ava_database::{DynDatabase, MemDb};
use ava_platformvm::txs::components::Output as POutput;
use ava_platformvm::utxo::Utxo as PUtxo;
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::components::avax::shared_memory::{Element, Requests, SharedMemory};

const NETWORK_ID: u32 = 10;
const TX_FEE: u64 = 0;
const CREATE_ASSET_TX_FEE: u64 = 0;
const NUM_FXS: usize = 3;

/// The X-Chain id used by the X view of shared memory.
fn x_chain_id() -> Id {
    Id::from([0x05; 32])
}

/// The P-Chain id used by the P view of shared memory (the export destination).
fn p_chain_id() -> Id {
    Id::from([0x22; 32])
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
        x_chain_id(),
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
            blockchain_id: x_chain_id(),
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

fn seed_utxo(tx_byte: u8, idx: u32, asset_id: Id, amt: u64) -> (Id, Vec<u8>) {
    let mut tx_id = [0u8; 32];
    tx_id[0] = tx_byte;
    let utxo = AvmUtxo {
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

/// Reads the committed `value_hex` field of an atomic Go vector.
fn vector_value_hex(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/vectors/atomic/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).expect("read vector json");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("parse vector json");
    let hexed = json["value_hex"].as_str().expect("value_hex field");
    hex::decode(hexed).expect("decode value_hex")
}

mod differential {
    use super::*;

    /// `differential::atomic_xp` — the recorded-oracle ATOMIC-1 gate.
    ///
    /// Drives BOTH directions of an X↔P atomic UTXO transfer through the REAL
    /// `ava-chains` shared-memory channel and asserts the cross-chain byte
    /// contract against the committed Go-derived vectors.
    #[test]
    fn atomic_xp() {
        x_to_p_export_decode();
        p_to_x_export_decode();
    }

    // -------------------------------------------------------------------------
    // X → P export decode (ATOMIC-1 — the core assertion)
    // -------------------------------------------------------------------------
    //
    // Build an X-Chain ExportTx (destination = P-Chain) over a seeded UTXO set,
    // run it through a `BlockManager::accept` of a StandardBlock with a REAL
    // `ava-chains` SharedMemoryView for the X-Chain as the backend. Assert:
    //   (a) the emitted Element.value equals the committed Go vector hex;
    //   (b) the P-Chain's SharedMemoryView `get` returns the same bytes AND
    //       `ava_platformvm::utxo::Utxo::unmarshal` decodes an identical UTXO.
    fn x_to_p_export_decode() {
        // ONE base DB backs BOTH the X-Chain state AND the `Memory` so the
        // accept-path atomic co-commit (`commit_batch_ops` side batch written by
        // `SharedMemory::apply`) lands in the same DB the state reads from — the
        // production invariant (Go: one versiondb whose `CommitBatch` is
        // `WriteAll`'d with the atomic side batches, 27 §2.2). Each chain gets a
        // shared-memory view by its own chain_id.
        let base = Arc::new(MemDb::new());
        let memory = Memory::new(Arc::clone(&base) as Arc<dyn DynDatabase>);
        let sm_x = memory.new_shared_memory(x_chain_id());
        let sm_p = memory.new_shared_memory(p_chain_id());

        let genesis_id = Id::from([0x11; 32]);

        // The asset to export.
        let ca = create_asset_tx();
        let asset_id = ca.id();
        let (utxo_id, utxo_bytes) = seed_utxo(0x33, 0, asset_id, 3000);

        // Build the X-Chain BlockManager over the SAME base DB as `Memory`,
        // wired with the REAL X-Chain shared-memory view as its atomic backend.
        let mut state = State::new(Arc::clone(&base)).expect("state");
        state.set_last_accepted(genesis_id);
        state.add_tx(asset_id, ca.bytes().to_vec());
        state.add_utxo(utxo_id, utxo_bytes);
        state.commit().expect("commit genesis");

        let cfg = BlockManagerConfig {
            backend: backend(),
            dispatch: dispatch(),
            shared_memory: Arc::new(sm_x) as Arc<dyn SharedMemory>,
        };
        let mut mgr = BlockManager::new(state, cfg);

        // ExportTx: consume the seeded UTXO, export one output to the P-Chain.
        // base.outs is empty, so the exported output gets output_index 0.
        let export_tx = signed(
            UnsignedTx::Export(ExportTx {
                base: BaseTx::new(AvaxBaseTx {
                    network_id: NETWORK_ID,
                    blockchain_id: x_chain_id(),
                    outs: Vec::new(),
                    ins: vec![transfer_input(0x33, 0, asset_id, 3000)],
                    memo: Vec::new(),
                }),
                destination_chain: p_chain_id(),
                exported_outs: vec![transfer_output(asset_id, 3000)],
            }),
            1,
        );
        let tx_id = export_tx.id();

        let c = ava_avm::txs::codec::codec().expect("codec");
        let blk = StandardBlock::new_block(&c, genesis_id, 1, 1_000_000, vec![export_tx])
            .expect("build block");

        mgr.verify(&blk).expect("verify export");

        // Recompute the expected exported UTXO + its shared-memory key BEFORE
        // accept (which moves the cached requests into shared memory).
        let exported = AvmUtxo {
            tx_id,
            output_index: 0,
            asset_id,
            out: Output::SecpTransfer(TransferOutput::new(3000, owners())),
        };
        let exported_value = exported.marshal().expect("marshal exported");
        let exported_key = exported.input_id().to_bytes().to_vec();

        // (a) Byte-exact Go-vector assertion. The exported UTXO's `tx_id` is
        // tx-derived (not fixed), so the committed vector pins a fixed-field UTXO
        // with the IDENTICAL byte layout. Build that exact UTXO and assert its
        // avm-codec marshalling equals the committed Go vector hex byte-for-byte;
        // this pins the avm `avax.UTXO` v0 wire layout + secp type_id-7 output.
        let vector_utxo = AvmUtxo {
            tx_id: Id::from([0x42; 32]),
            output_index: 1,
            asset_id: Id::from([0x77; 32]),
            out: Output::SecpTransfer(TransferOutput::new(1000, owners())),
        };
        assert_eq!(
            vector_utxo.marshal().expect("marshal vector utxo"),
            vector_value_hex("x_to_p_utxo"),
            "X→P UTXO avm-codec bytes must match the Go vector byte-for-byte"
        );

        // And the executor-emitted bytes share that exact layout (same length,
        // same `0x0000` version prefix, same trailing 20-byte owner address).
        let vec_value = vector_value_hex("x_to_p_utxo");
        assert_eq!(
            exported_value.len(),
            vec_value.len(),
            "executor-emitted UTXO byte length must match the Go-vector layout"
        );
        assert_eq!(
            &exported_value[..2],
            &vec_value[..2],
            "codec version 0x0000"
        );
        assert_eq!(
            &exported_value[exported_value.len() - 20..],
            &vec_value[vec_value.len() - 20..],
            "trailing owner address (trait) byte layout must match the Go vector"
        );

        mgr.accept(&blk).expect("accept export");

        // (b) Fetch the exported value back from the P-Chain's inbound view —
        // this exercises the REAL shared-memory channel (sharedID prefix +
        // dbElement framing), proving the round-trip, not just marshal/unmarshal.
        let got = sm_p
            .get(x_chain_id(), std::slice::from_ref(&exported_key))
            .expect("p-chain get exported utxo");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0], exported_value,
            "bytes read by the P-Chain view must equal the X-Chain export bytes"
        );

        // The CORE ATOMIC-1 assertion: the P-Chain codec decodes the X-exported
        // bytes into an identical UTXO (same tx_id / output_index / asset_id /
        // secp output) — cross-chain decode through the shared type_id 7.
        let p_decoded = PUtxo::unmarshal(&got[0]).expect("p-chain unmarshal x-exported utxo");
        assert_eq!(
            p_decoded.tx_id, tx_id,
            "tx_id must cross-decode identically"
        );
        assert_eq!(p_decoded.output_index, 0, "output_index must cross-decode");
        assert_eq!(
            p_decoded.asset_id, asset_id,
            "asset_id must cross-decode identically"
        );
        assert_eq!(
            p_decoded.out,
            POutput::Transfer(TransferOutput::new(3000, owners())),
            "secp TransferOutput must cross-decode identically (shared type_id 7)"
        );

        // Re-marshalling the P-decoded UTXO reproduces the exact X bytes
        // (byte-exact round-trip through both codecs).
        assert_eq!(
            p_decoded.marshal().expect("p-chain re-marshal"),
            exported_value,
            "P-Chain re-marshal must reproduce the X-Chain export bytes byte-for-byte"
        );

        // The input UTXO is gone from the X-Chain's persisted state.
        assert!(
            mgr.state().get_utxo(utxo_id).is_err(),
            "the consumed input UTXO must be deleted after accept"
        );
    }

    // -------------------------------------------------------------------------
    // P → X export decode (reverse direction)
    // -------------------------------------------------------------------------
    //
    // The P-Chain exports a UTXO (hand-built P-Chain `Utxo::marshal`, mirroring
    // its export path); we apply it to the REAL shared-memory channel through the
    // P-Chain view, then assert the X-Chain view reads identical bytes and the
    // avm `Utxo::unmarshal` decodes it identically. Also pins the Go vector.
    fn p_to_x_export_decode() {
        let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
        let memory = Memory::new(Arc::clone(&base));
        let sm_p = memory.new_shared_memory(p_chain_id());
        let sm_x = memory.new_shared_memory(x_chain_id());

        // A P-Chain UTXO matching the committed Go vector (fixed fields).
        let p_utxo = PUtxo {
            tx_id: Id::from([0x91; 32]),
            output_index: 3,
            asset_id: Id::from([0x55; 32]),
            out: POutput::Transfer(TransferOutput::new(
                250_000,
                OutputOwners::new(7, 1, vec![ShortId::from([0xcd; 20])]),
            )),
        };
        let p_value = p_utxo.marshal().expect("p-chain marshal");
        let p_key = p_utxo.input_id().to_bytes().to_vec();

        // The P-Chain value bytes must match the committed Go vector exactly.
        assert_eq!(
            p_value,
            vector_value_hex("p_to_x_utxo"),
            "P→X exported UTXO bytes must match the Go vector"
        );

        // The P-Chain applies the export: put the Element destined for the
        // X-Chain through the REAL shared-memory channel.
        let mut reqs: BTreeMap<Id, Requests> = BTreeMap::new();
        reqs.insert(
            x_chain_id(),
            Requests {
                remove: Vec::new(),
                put: vec![Element {
                    key: p_key.clone(),
                    value: p_value.clone(),
                    traits: vec![ShortId::from([0xcd; 20]).to_bytes().to_vec()],
                }],
            },
        );
        sm_p.apply(reqs, &[]).expect("p-chain apply export");

        // The X-Chain reads the exported bytes from its inbound view.
        let got = sm_x
            .get(p_chain_id(), &[p_key])
            .expect("x-chain get p-exported utxo");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0], p_value,
            "bytes read by the X-Chain view must equal the P-Chain export bytes"
        );

        // The CORE reverse assertion: the avm codec decodes the P-exported bytes
        // into an identical UTXO.
        let x_decoded = AvmUtxo::unmarshal(&got[0]).expect("avm unmarshal p-exported utxo");
        assert_eq!(x_decoded.tx_id, Id::from([0x91; 32]));
        assert_eq!(x_decoded.output_index, 3);
        assert_eq!(x_decoded.asset_id, Id::from([0x55; 32]));
        assert_eq!(
            x_decoded.out,
            Output::SecpTransfer(TransferOutput::new(
                250_000,
                OutputOwners::new(7, 1, vec![ShortId::from([0xcd; 20])]),
            )),
            "secp TransferOutput must cross-decode identically (shared type_id 7)"
        );

        // Re-marshalling through the avm codec reproduces the exact P bytes.
        assert_eq!(
            x_decoded.marshal().expect("avm re-marshal"),
            p_value,
            "avm re-marshal must reproduce the P-Chain export bytes byte-for-byte"
        );
    }
}
