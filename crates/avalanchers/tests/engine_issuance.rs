// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 STEP (m) — **engine-driven block issuance with a real `PlatformVm`,
//! across a restart.** Closes the STEP (l) follow-up: rather than driving
//! `build → verify → accept` directly on the VM (the reexecute `replay_pchain`
//! path), this admits a funded tx and lets the running **Snowman engine** build,
//! issue, and accept a real height-1 P-Chain block through the genuine consensus
//! poll path — made possible by the self-loopback `Sender` (a solo node's
//! `k=1`/`β=1` poll self-resolves). It then proves the engine-issued tip
//! **persists and resumes**: a fresh node re-booted over the same base db comes
//! up rooted at the advanced height, not genesis.
//!
//! The funded synthetic genesis + signed `CreateSubnetTx` mirror the
//! `ava-reexecute` P-Chain leg (`tests/reexecute/src/pchain.rs`): two genesis
//! UTXOs owned by a seed-derived key, one future-pinned Primary-Network
//! validator (so no staker-change cap caps the block time), and a
//! `CreateSubnetTx` spending the first UTXO. The VM's injected [`MockClock`] is
//! pinned to the (far-future) genesis timestamp so `build_block` stamps the
//! height-1 block deterministically (specs 24 hazard #5). The proposervm wrapper
//! the chain pipeline adds is **pre-fork pass-through** here — the in-process
//! boot clock sits at the Unix epoch, before any fork — so `build_block` reaches
//! the inner `PlatformVm` directly (no proposer windowing).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_database::{DynDatabase, MemDb};
use ava_node::init::chain_manager::PLATFORM_CHAIN_ID;
use ava_platformvm::genesis::{Genesis, GenesisUtxo, Utxo as GenesisAvaxUtxo};
use ava_platformvm::signer::{ProofOfPossession, Signer};
use ava_platformvm::txs::base_tx::BaseTx;
use ava_platformvm::txs::components::{
    BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
};
use ava_platformvm::txs::fee::complexity::base_tx_complexity;
use ava_platformvm::txs::fee::dynamic_calculator::DynamicCalculator;
use ava_platformvm::txs::validator::Validator;
use ava_platformvm::txs::{
    AddPermissionlessValidatorTx, Codec, CreateSubnetTx, GenesisCodec, Tx, UnsignedTx,
};
use ava_platformvm::vm::PlatformVm;
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_snow::EngineState;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_utils::clock::MockClock;
use ava_vm::vm::VmEvent;
use avalanchers::wiring::chains::{PChainBootHandle, boot_chain_with_loopback};

// ---------------------------------------------------------------------------
// Funded synthetic genesis + tx (ported from the ava-reexecute P-Chain leg).
// ---------------------------------------------------------------------------

const NETWORK_ID: u32 = 1;

/// The AVAX asset id every seeded UTXO is denominated in (the Go-vector asset id).
const AVAX_ASSET_ID: [u8; 32] = [
    0x21, 0xe6, 0x73, 0x17, 0xcb, 0xc4, 0xbe, 0x2a, 0xeb, 0x00, 0x67, 0x7a, 0xd6, 0x46, 0x27, 0x78,
    0xa8, 0xf5, 0x22, 0x74, 0xb9, 0xd6, 0x05, 0xdf, 0x25, 0x91, 0xb2, 0x30, 0x27, 0xa8, 0x7d, 0xff,
];

/// A valid BLS compressed public key + proof-of-possession (the synthetic
/// permissionless-validator PoP signer; only stake amounts/period vary by seed).
const BLS_PUBKEY: [u8; 48] = [
    0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3, 0xa4,
    0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b, 0x90, 0xcf,
    0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e, 0xd0, 0xd4, 0xd1,
];
const BLS_SIG: [u8; 96] = [
    0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20, 0x7b,
    0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83, 0x46, 0xd9,
    0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b, 0x2a, 0x05, 0xe0,
    0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a, 0x18, 0xa0, 0x83, 0xe4,
    0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d, 0xa5, 0xc4, 0x89, 0x87, 0x2e,
    0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33, 0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
];

/// The genesis chain time (year ~2255), matched by the VM's injected clock so
/// `next_block_time` resolves to `max(now, parent_ts) = GENESIS_TS` with no cap.
const GENESIS_TS: u64 = 9_000_000_000;

/// A tiny deterministic bit-mixer (splitmix64 finalizer).
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

fn owner_addr(seed: u64) -> ShortId {
    let mut a = [0u8; 20];
    a[..8].copy_from_slice(&mix(seed).to_be_bytes());
    a[8..16].copy_from_slice(&mix(seed.wrapping_add(0x1111)).to_be_bytes());
    ShortId::from(a)
}

fn owners(seed: u64) -> OutputOwners {
    OutputOwners::new(0, 1, vec![owner_addr(seed)])
}

/// The seed-derived amount of the first genesis UTXO `U0` (spent by the tx).
fn genesis_amount0(seed: u64) -> u64 {
    (mix(seed) % 900_000_000).saturating_add(100_000_000)
}

fn genesis_amount1(seed: u64) -> u64 {
    (mix(seed.wrapping_add(0xABCD)) % 900_000_000).saturating_add(100_000_000)
}

/// Builds the seed-derived synthetic genesis (two UTXOs + one future-pinned
/// Primary-Network validator). Pure in `seed`, so the marshalled bytes are stable.
fn build_genesis(seed: u64) -> Genesis {
    let avax = Id::from(AVAX_ASSET_ID);
    let amount0 = genesis_amount0(seed);
    let amount1 = genesis_amount1(seed);
    let stake = (mix(seed.wrapping_add(0x5555)) % 1_000_000_000).saturating_add(2_000_000_000);

    let utxo0 = GenesisUtxo {
        utxo: GenesisAvaxUtxo {
            tx_id: Id::EMPTY,
            output_index: 0,
            asset_id: avax,
            out: Output::Transfer(TransferOutput::new(amount0, owners(seed))),
        },
        message: vec![],
    };
    let utxo1 = GenesisUtxo {
        utxo: GenesisAvaxUtxo {
            tx_id: Id::EMPTY,
            output_index: 1,
            asset_id: avax,
            out: Output::Transfer(TransferOutput::new(amount1, owners(seed))),
        },
        message: vec![],
    };

    let node_seed = mix(seed.wrapping_add(0x7777));
    let mut node_bytes = [0u8; 20];
    node_bytes[..8].copy_from_slice(&node_seed.to_be_bytes());
    node_bytes[8..16].copy_from_slice(&mix(node_seed).to_be_bytes());

    let mut vdr = Tx::new(UnsignedTx::AddPermissionlessValidator(
        AddPermissionlessValidatorTx {
            base: BaseTx::new(AvaxBaseTx {
                network_id: NETWORK_ID,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![TransferableInput {
                    tx_id: Id::EMPTY,
                    output_index: 0,
                    asset_id: avax,
                    r#in: Input::Transfer(TransferInput::new(stake, vec![0])),
                }],
                memo: vec![],
            }),
            validator: Validator {
                node_id: NodeId::from(node_bytes),
                start: GENESIS_TS,
                end: GENESIS_TS + 30 * 24 * 60 * 60,
                wght: stake,
            },
            subnet: Id::EMPTY,
            signer: Signer::ProofOfPossession(ProofOfPossession::new(BLS_PUBKEY, BLS_SIG)),
            stake_outs: vec![TransferableOutput {
                asset_id: avax,
                out: Output::Transfer(TransferOutput::new(stake, owners(seed))),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners(seed)),
            delegator_rewards_owner: Owner::Secp256k1(owners(seed)),
            delegation_shares: 1_000_000,
            verified: std::cell::OnceCell::new(),
        },
    ));
    vdr.initialize(GenesisCodec())
        .expect("initialize genesis validator tx");

    Genesis {
        utxos: vec![utxo0, utxo1],
        validators: vec![vdr],
        chains: vec![],
        timestamp: GENESIS_TS,
        initial_supply: 360_000_000u64.saturating_mul(1_000_000_000),
        message: "engine-issuance synthetic genesis".to_string(),
    }
}

/// The post-Etna dynamic fee charged for a decision tx at the genesis tip.
fn create_subnet_fee() -> u64 {
    DynamicCalculator::from_excess(0)
        .calculate_fee(base_tx_complexity())
        .expect("compute create-subnet fee")
}

/// A signed, initialized `CreateSubnetTx` spending genesis UTXO `(tx_id, idx)`
/// holding `amount` (the input credential is not signature-checked while the VM
/// runs un-bootstrapped, exactly as the reexecute leg).
fn create_subnet_tx_spending(seed: u64, tx_id: Id, output_index: u32, amount: u64) -> Tx {
    let avax = Id::from(AVAX_ASSET_ID);
    let fee = create_subnet_fee();
    let change = amount
        .checked_sub(fee)
        .expect("genesis UTXO covers the create-subnet fee");

    let tx = CreateSubnetTx {
        base: BaseTx::new(AvaxBaseTx {
            network_id: NETWORK_ID,
            blockchain_id: Id::EMPTY,
            outs: vec![TransferableOutput {
                asset_id: avax,
                out: Output::Transfer(TransferOutput::new(change, owners(seed))),
            }],
            ins: vec![TransferableInput {
                tx_id,
                output_index,
                asset_id: avax,
                r#in: Input::Transfer(TransferInput::new(amount, vec![0])),
            }],
            memo: vec![],
        }),
        owner: Owner::Secp256k1(owners(seed)),
    };

    let mut tx = Tx::new(UnsignedTx::CreateSubnet(tx));
    tx.initialize(Codec()).expect("initialize create-subnet tx");
    tx
}

// ---------------------------------------------------------------------------
// Test helpers.
// ---------------------------------------------------------------------------

/// A `PlatformVm` whose injected clock is pinned to the genesis timestamp, with a
/// single funded `CreateSubnetTx` (spending `U0`) pre-loaded into its mempool —
/// ready for the engine to drain into a height-1 block once it reaches NormalOp.
fn funded_pchain_vm(seed: u64) -> PlatformVm {
    let pinned = UNIX_EPOCH
        .checked_add(Duration::from_secs(GENESIS_TS))
        .expect("genesis timestamp within SystemTime range");
    let vm = PlatformVm::with_clock(Arc::new(MockClock::at(pinned)));
    // `mempool_add` is a holding pen (no init / no state validation), so the tx
    // can be admitted before the VM is moved into the chain; the builder
    // validates it against the seeded genesis state at build time.
    vm.mempool_add(create_subnet_tx_spending(
        seed,
        Id::EMPTY,
        0,
        genesis_amount0(seed),
    ))
    .expect("admit the funded CreateSubnetTx to the mempool");
    vm
}

/// Boot the P-Chain through the loopback engine path over `base_db`, with `vm` as
/// the inner VM.
async fn boot(
    vm: PlatformVm,
    genesis_bytes: Vec<u8>,
    base_db: Arc<dyn DynDatabase>,
) -> PChainBootHandle {
    let genesis_id = ava_platformvm::genesis::genesis_id(&genesis_bytes);
    boot_chain_with_loopback(
        NETWORK_ID,
        PLATFORM_CHAIN_ID,
        ava_types::constants::PRIMARY_NETWORK_ID,
        "P",
        Id::from(AVAX_ASSET_ID),
        genesis_id,
        vm,
        genesis_bytes,
        base_db,
    )
    .await
    .expect("boot the P-Chain through the loopback engine path")
}

/// Snapshot every (key, value) in the base db (the persistence observation).
fn snapshot(db: &Arc<dyn DynDatabase>) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut it = db.new_iterator_with_start_and_prefix(&[], &[]);
    let mut out = BTreeMap::new();
    while it.next() {
        match (it.key(), it.value()) {
            (Some(k), Some(v)) => {
                out.insert(k.to_vec(), v.to_vec());
            }
            _ => break,
        }
    }
    out
}

async fn await_normalop(handle: &PChainBootHandle) {
    for _ in 0..400_000 {
        if matches!(**handle.ctx.state.load(), EngineState::NormalOp) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "the solo engine reached NormalOp (last state: {:?})",
        **handle.ctx.state.load()
    );
}

// ---------------------------------------------------------------------------
// The test.
// ---------------------------------------------------------------------------

/// M9.15 STEP (m) — the engine issues a real P-Chain block and the tip resumes
/// across a restart. Boot a funded `PlatformVm` through the loopback engine path,
/// drive to NormalOp, signal `PendingTxs` (the engine builds + issues + accepts a
/// height-1 block), then re-boot a fresh node over the **same** base db and
/// assert it resumes rooted at height 1 — the engine-issued tip, not genesis.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn engine_issued_pchain_tip_resumes_after_restart() {
    let seed = 7u64;
    let genesis_bytes =
        ava_platformvm::genesis::marshal(&build_genesis(seed)).expect("marshal genesis");

    // The node's single persistent base db; the Arc survives the restart just as
    // an on-disk backend survives a process restart.
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());

    // ---- Boot 1: a fresh node over the empty base db, with the funded tx queued.
    let handle1 = boot(
        funded_pchain_vm(seed),
        genesis_bytes.clone(),
        Arc::clone(&base),
    )
    .await;
    assert_eq!(
        handle1.last_accepted_height, 0,
        "boot 1 roots consensus at the genesis tip (height 0)"
    );
    await_normalop(&handle1).await;

    // Genesis state is flushed; snapshot it as the pre-issuance baseline.
    let before = snapshot(&base);

    // Signal pending txs: the engine builds + issues + (via the loopback poll)
    // accepts a real height-1 BanffStandardBlock spending the genesis UTXO.
    handle1
        .vm_tx
        .send(VmEvent::PendingTxs)
        .await
        .expect("signal PendingTxs to the running engine");

    // Wait until the accepted block + its diff are flushed to the base db (the
    // observable acceptance signal — a self-built block stuck *processing* without
    // the loopback would never grow the db, so this also guards the RED case).
    let mut issued = false;
    for _ in 0..3_000_000 {
        if snapshot(&base).len() > before.len() {
            issued = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        issued,
        "the engine accepted a height-1 block and flushed it to the base db"
    );

    // Clean shutdown, keeping the base db alive (the on-disk state a restart
    // re-opens).
    handle1.token.cancel();
    handle1.join.await.expect("boot 1 handler task joined");

    // ---- Boot 2: restart — a fresh node over the SAME base db, no queued tx.
    let handle2 = boot(
        PlatformVm::with_clock(Arc::new(MockClock::at(
            UNIX_EPOCH
                .checked_add(Duration::from_secs(GENESIS_TS))
                .expect("genesis ts in range"),
        ))),
        genesis_bytes,
        Arc::clone(&base),
    )
    .await;

    // The engine-issued tip resumed: consensus is rooted at the advanced height,
    // not genesis. (Without the loopback, boot 1 never accepts, so the persisted
    // tip stays genesis and this would be 0.)
    assert_eq!(
        handle2.last_accepted_height, 1,
        "the restart resumes the engine-issued height-1 tip, not genesis"
    );

    handle2.token.cancel();
    handle2.join.await.expect("boot 2 handler task joined");
}
