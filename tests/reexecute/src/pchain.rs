// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain (`ava-platformvm`) reexecute leg (specs/02 §10.5/§11.1, specs/16
//! §5(3), specs/00 §11.7).
//!
//! [`replay_pchain`] drives a synthetic-but-real reexecute case through the REAL
//! `ava-platformvm` VM/block pipeline and returns its deterministic roots
//! ([`PchainReexecuteRoots`]). It is the P-Chain analogue of the C-Chain leg's
//! [`replay_cchain`](crate::replay_cchain) and the X-Chain leg's
//! [`replay_xchain`](crate::replay_xchain): just as `genesis_to_1` is a synthetic
//! fixture run through the real EVM pipeline, this builds a seed-derived P-Chain
//! genesis and runs it through the genuine VM execution path (parse + seed genesis
//! state → `build → set_preference → verify → accept` until the VM declines).
//!
//! ## What is reached (and what is NOT) — the honest pipeline floor
//!
//! Unlike the X-Chain leg, the P-Chain mempool is **un-shared** on `PlatformVm`
//! (`vm.rs` ~line 166: "RPC issuance not yet wired") — there is no public seam to
//! admit a decision tx, and admitting one without patching the VM was deliberately
//! avoided. The only height-advancing control flow reachable WITHOUT a decision tx
//! is the builder's reward-proposal path (`getNextStakerToReward` →
//! `BanffProposalBlock`/`RewardValidatorTx`), but that path is itself **not yet
//! reachable through genesis seeding**: `genesis::seed_state` records the genesis
//! validator as a current *staker* but does NOT store its tx in the tx store, so
//! the reward executor's `staker_tx_resolver` (`state.GetTx`) returns
//! `database: ErrNotFound` on verify (a latent gap in the genesis ⇄ staker-reward
//! wiring — the M4.24 reward-wiring follow-up — NOT something this test should
//! paper over by patching production genesis behaviour). See `tests/PORTING.md`.
//!
//! So this leg drives the REAL pipeline to the honestly-reachable floor: it
//! `initialize`s the VM over a seed-derived genesis (parse → `seed_state` → genesis
//! block), then calls `build_block`. With the genesis chain time + the validator
//! period pinned FAR in the FUTURE (so `now < parent_ts`, the X-Chain leg's
//! clock-pinning trick), `next_block_time` resolves to the fixed genesis time with
//! no staker-change cap, so the builder declines (`ErrNoPendingBlocks`) — the
//! genuine terminal state, wall-clock-independent. The chain stays at the accepted
//! genesis block (height 0). Every step is REAL VM code (no fabricated/hardcoded
//! root); what is NOT yet reached is a height >= 1 accepted block (blocked on the
//! decision-tx mempool seam AND the genesis ⇄ reward-resolver gap above).
//!
//! The seed varies the genesis state (UTXO amounts/owner, initial supply,
//! validator stake), so the genesis post-state digest is seed-dependent and a
//! different seed yields a different root — the determinism assertion genuinely
//! catches divergence rather than passing on a constant.
//!
//! ## The digest (no merkle root)
//!
//! The P-Chain keeps FLAT KV state (no merkledb — `state/`), so — exactly like the
//! X-Chain leg — the reexecute "root" is the deterministic POST-STATE DIGEST: a
//! `sha256` over the canonically-sorted final UTXO set (enumerated by the genesis
//! owner address via `State::utxo_ids`), the Primary-Network current supply, and
//! the chain timestamp, alongside the chain-tip block id + height. Two replays of
//! the same seed produce byte-identical roots — the determinism / reproducibility
//! property the recorded-oracle path proves WITHOUT a live Go oracle. The Go
//! recorded-oracle parity arm (and the height >= 1 accepted-block arm) are the
//! follow-ups (see `tests/PORTING.md`).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use async_trait::async_trait;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use ava_crypto::hashing;
use ava_database::{DynDatabase, MemDb};
use ava_platformvm::genesis::{Genesis, GenesisUtxo, Utxo as GenesisAvaxUtxo};
use ava_platformvm::signer::{ProofOfPossession, Signer};
use ava_platformvm::state::chain::Chain;
use ava_platformvm::state::state::State;
use ava_platformvm::txs::base_tx::BaseTx;
use ava_platformvm::txs::components::{
    BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
};
use ava_platformvm::txs::validator::Validator;
use ava_platformvm::txs::{AddPermissionlessValidatorTx, GenesisCodec, Tx, UnsignedTx};
use ava_platformvm::vm::{DynDb, PlatformVm};
use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;

use crate::{Error, Result};

/// The deterministic roots a single P-Chain reexecute case produces.
///
/// `last_accepted_id` is the chain-tip block id after replaying the case (the
/// P-Chain analogue of a state/merkle root the reexecute oracle keys on);
/// `state_digest` is the `sha256` over the canonically-sorted final UTXO set plus
/// the supply + chain timestamp (the post-state digest, since the P-Chain keeps no
/// merkle trie). Two replays of the same seed must produce an identical
/// [`PchainReexecuteRoots`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PchainReexecuteRoots {
    /// The chain-tip (last-accepted) block id after the replay.
    pub last_accepted_id: [u8; 32],
    /// The chain-tip block height after the replay (genesis is `0`).
    pub last_accepted_height: u64,
    /// `sha256` over the canonically-sorted final UTXO set + Primary-Network
    /// supply + chain timestamp — the deterministic post-state digest.
    pub state_digest: [u8; 32],
}

// ---------------------------------------------------------------------------
// Fixed chain identity + synthetic-genesis constants.
// ---------------------------------------------------------------------------

const NETWORK_ID: u32 = 1;

/// The AVAX asset id every seeded UTXO is denominated in (the Go-vector asset id).
const AVAX_ASSET_ID: [u8; 32] = [
    0x21, 0xe6, 0x73, 0x17, 0xcb, 0xc4, 0xbe, 0x2a, 0xeb, 0x00, 0x67, 0x7a, 0xd6, 0x46, 0x27, 0x78,
    0xa8, 0xf5, 0x22, 0x74, 0xb9, 0xd6, 0x05, 0xdf, 0x25, 0x91, 0xb2, 0x30, 0x27, 0xa8, 0x7d, 0xff,
];

/// The BLS compressed public key + proof-of-possession from the Go vectors (the
/// synthetic permissionless-validator PoP signer — fixed across seeds: only the
/// staking *amounts*/period vary, so the PoP stays a valid, parseable signer).
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

/// The genesis chain time, pinned FAR in the FUTURE (year ~2255) relative to any
/// real run. The X-Chain leg's clock-pinning trick: because `now < GENESIS_TS`,
/// `next_block_time` resolves to `max(now, parent_ts) = parent_ts = GENESIS_TS`
/// with no wall-clock leak, and (with the validator period also future-pinned, see
/// [`build_genesis`]) no staker-change cap fires — so the builder declines
/// (`ErrNoPendingBlocks`) at the genesis tip, deterministically across runs.
const GENESIS_TS: u64 = 9_000_000_000;

/// A tiny deterministic bit-mixer (splitmix64 finalizer) — pure, no global state.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The seed-derived genesis owner address (one fixed-by-seed 20-byte address that
/// every genesis UTXO + the staker rewards owner reference; the digest enumerates
/// the UTXO set by this address).
fn owner_addr(seed: u64) -> ShortId {
    let mut a = [0u8; 20];
    a[..8].copy_from_slice(&mix(seed).to_be_bytes());
    a[8..16].copy_from_slice(&mix(seed.wrapping_add(0x1111)).to_be_bytes());
    ShortId::from(a)
}

fn owners(seed: u64) -> OutputOwners {
    OutputOwners::new(0, 1, vec![owner_addr(seed)])
}

/// Builds the seed-derived synthetic [`Genesis`]: two genesis UTXOs (seed-varied
/// amounts), one Primary-Network permissionless validator with a seed-varied stake
/// and a fixed PAST end time, an initial supply, and the genesis timestamp. Every
/// field is a pure function of `seed`, so the marshalled bytes (and thus the
/// genesis id, block id, and seeded state) are byte-identical across runs.
fn build_genesis(seed: u64) -> Result<Genesis> {
    let avax = Id::from(AVAX_ASSET_ID);
    let amount0 = (mix(seed) % 900_000_000).saturating_add(100_000_000);
    let amount1 = (mix(seed.wrapping_add(0xABCD)) % 900_000_000).saturating_add(100_000_000);
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

    // A single Primary-Network permissionless validator. `start`/`end` are pinned
    // in the FUTURE (start = GENESIS_TS, end = GENESIS_TS + 30 days), so the next
    // staker-change time is `> parent_ts` and never caps the new-block time — the
    // builder declines at genesis rather than emitting a (currently un-resolvable)
    // reward block. Wall-clock-independent and deterministic.
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
        .map_err(|e| Error::Pchain(format!("initialize validator tx: {e}")))?;

    Ok(Genesis {
        utxos: vec![utxo0, utxo1],
        validators: vec![vdr],
        chains: vec![],
        timestamp: GENESIS_TS,
        initial_supply: 360_000_000u64.saturating_mul(1_000_000_000),
        message: "reexecute synthetic genesis".to_string(),
    })
}

// ---------------------------------------------------------------------------
// The VM-driving harness.
// ---------------------------------------------------------------------------

/// A no-op [`AppSender`] for `initialize`.
#[derive(Debug, Default)]
struct NoopAppSender;

#[async_trait]
impl AppSender for NoopAppSender {
    async fn send_app_request(
        &self,
        _token: &CancellationToken,
        _nodes: &HashSet<NodeId>,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
    async fn send_app_response(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
    async fn send_app_error(
        &self,
        _token: &CancellationToken,
        _node: NodeId,
        _request_id: u32,
        _code: i32,
        _message: &str,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
    async fn send_app_gossip(
        &self,
        _token: &CancellationToken,
        _config: SendConfig,
        _bytes: Vec<u8>,
    ) -> ava_vm::error::Result<()> {
        Ok(())
    }
}

fn chain_ctx() -> Arc<ChainContext> {
    Arc::new(ChainContext {
        network_id: NETWORK_ID,
        subnet_id: Id::EMPTY,
        chain_id: Id::EMPTY,
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(NETWORK_ID),
        x_chain_id: Id::EMPTY,
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::from(AVAX_ASSET_ID),
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// The bound on accepted blocks (defensive — the real control flow declines via
/// `ErrNoPendingBlocks` well before this; the cap only guards against an
/// unexpected non-terminating build loop).
const MAX_BLOCKS: usize = 16;

/// Replay a synthetic seed-derived P-Chain reexecute case through the REAL
/// `ava-platformvm` VM/block pipeline and return its deterministic roots.
///
/// Seeds a seed-derived genesis (two UTXOs + one current validator), then drives
/// `build → set_preference → verify → accept` until the builder declines
/// (`ErrNoPendingBlocks`). The returned [`PchainReexecuteRoots`] carries the
/// chain-tip block id + height and the `sha256` post-state digest over the sorted
/// final UTXO set + supply + chain time.
///
/// # Errors
/// Returns an [`Error::Pchain`] if any VM/codec step fails (build genesis,
/// initialize, build, verify, accept, or the post-state read).
pub fn replay_pchain(seed: u64) -> Result<PchainReexecuteRoots> {
    // One multi-thread runtime per call keeps each VM instance fully independent
    // (the reexecute determinism gate replays the same case twice and compares).
    let rt = Runtime::new().map_err(|e| Error::Pchain(format!("tokio runtime: {e}")))?;
    rt.block_on(replay_pchain_async(seed))
}

async fn replay_pchain_async(seed: u64) -> Result<PchainReexecuteRoots> {
    let token = CancellationToken::new();
    let mut vm = PlatformVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());

    let genesis = build_genesis(seed)?;
    let genesis_bytes = ava_platformvm::genesis::marshal(&genesis)
        .map_err(|e| Error::Pchain(format!("marshal genesis: {e}")))?;

    vm.initialize(
        &token,
        chain_ctx(),
        db,
        &genesis_bytes,
        b"",
        b"",
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .map_err(|e| Error::Pchain(format!("initialize: {e}")))?;

    // Drive build → set_preference → verify → accept until the builder declines
    // (`ErrNoPendingBlocks`, surfaced as a VM error). With the genesis time +
    // validator period future-pinned (see the module docs), the builder declines on
    // the FIRST call — so the chain stays at the accepted genesis tip (height 0).
    // The loop is written generally (and capped by `MAX_BLOCKS`) so that, once the
    // decision-tx mempool seam + genesis⇄reward-resolver wiring land, the same
    // driver advances height with no change here. No decision txs are issued (the
    // mempool is un-shared; see the module docs).
    let mut accepted = 0usize;
    while accepted < MAX_BLOCKS {
        let blk = match vm.build_block(&token).await {
            Ok(blk) => blk,
            // The builder declined (nothing pending + time need not advance) — the
            // terminal state. Any other error is fatal.
            Err(_) => break,
        };
        let blk_id = blk.id();
        vm.set_preference(&token, blk_id)
            .await
            .map_err(|e| Error::Pchain(format!("set_preference: {e}")))?;
        blk.verify(&token)
            .await
            .map_err(|e| Error::Pchain(format!("verify: {e}")))?;
        blk.accept(&token)
            .await
            .map_err(|e| Error::Pchain(format!("accept: {e}")))?;
        accepted = accepted.saturating_add(1);
    }

    // Capture the chain-tip block id + height.
    let last_id = vm
        .last_accepted(&token)
        .await
        .map_err(|e| Error::Pchain(format!("last_accepted: {e}")))?;
    let last_block = vm
        .get_block(&token, last_id)
        .await
        .map_err(|e| Error::Pchain(format!("get_block: {e}")))?;
    let last_accepted_height = last_block.height();

    // Read back the final state via the read-only seam: enumerate the genesis
    // owner's UTXO set (consumed/rewarded UTXOs drop out as the staker is processed),
    // plus the Primary-Network supply + chain timestamp. The `Chain` trait exposes
    // no global UTXO enumeration, so the address-keyed `utxo_ids` index is the
    // enumeration seam (genesis UTXOs are indexed by their owner address on seed).
    let addr = owner_addr(seed);
    let (mut survivors, supply, timestamp_secs) = vm
        .with_state(|state: &State<DynDb>| {
            let ids = state.utxo_ids(&addr, Id::EMPTY, usize::MAX);
            let mut out: Vec<(Id, Vec<u8>)> = Vec::with_capacity(ids.len());
            for id in ids {
                if let Ok(bytes) = Chain::get_utxo(state, id) {
                    out.push((id, bytes));
                }
            }
            let supply = Chain::current_supply(state, Id::EMPTY).unwrap_or(0);
            let timestamp_secs = Chain::timestamp(state)
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (out, supply, timestamp_secs)
        })
        .map_err(|e| Error::Pchain(format!("read state: {e}")))?;

    vm.shutdown(&token)
        .await
        .map_err(|e| Error::Pchain(format!("shutdown: {e}")))?;

    // The post-state digest: sha256 over the canonically-sorted (by id)
    // `(utxo_id ++ utxo_bytes)` concatenation, then the Primary-Network supply and
    // chain timestamp. Sorting by the 32-byte id makes the digest independent of
    // read-back order (no HashMap iteration leak, specs/00 §6.1).
    survivors.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut preimage = Vec::new();
    for (id, bytes) in &survivors {
        preimage.extend_from_slice(id.as_bytes());
        preimage.extend_from_slice(bytes);
    }
    preimage.extend_from_slice(&supply.to_be_bytes());
    preimage.extend_from_slice(&timestamp_secs.to_be_bytes());
    let state_digest = hashing::sha256(&preimage);

    let mut last_accepted_id = [0u8; 32];
    last_accepted_id.copy_from_slice(last_id.as_bytes());

    Ok(PchainReexecuteRoots {
        last_accepted_id,
        last_accepted_height,
        state_digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_is_seed_deterministic() {
        let a = ava_platformvm::genesis::marshal(&build_genesis(7).expect("genesis 7"))
            .expect("marshal a");
        let b = ava_platformvm::genesis::marshal(&build_genesis(7).expect("genesis 7"))
            .expect("marshal b");
        assert_eq!(a, b, "same seed must marshal byte-identical genesis");
    }

    #[test]
    fn replay_pchain_is_deterministic() {
        let a = replay_pchain(42).expect("first replay");
        let b = replay_pchain(42).expect("second replay");
        assert_eq!(a, b, "same case must produce identical roots");
        assert_ne!(a.state_digest, [0u8; 32], "real post-state digest");
    }

    #[test]
    fn perturbing_seed_changes_roots() {
        let a = replay_pchain(1).expect("replay seed 1");
        let b = replay_pchain(2).expect("replay seed 2");
        assert_ne!(a, b, "different cases should produce different roots");
    }
}
