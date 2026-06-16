// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (`ava-avm`) reexecute leg (specs/02 §10.5/§11.1, specs/16 §5(3),
//! specs/00 §11.7).
//!
//! [`replay_xchain`] drives a synthetic-but-real reexecute case through the REAL
//! `ava-avm` VM/block pipeline and returns its deterministic roots
//! ([`XchainReexecuteRoots`]). It is the X-Chain analogue of the C-Chain leg's
//! [`replay_cchain`](crate::replay_cchain): just as `genesis_to_1` is a synthetic
//! fixture run through the real EVM pipeline, this builds a seed-derived chain of
//! `BaseTx` issuances and runs them through the genuine VM execution path (seed
//! genesis state → admit txs → build → verify → accept).
//!
//! The X-Chain keys its UTXOs by id and does NOT maintain a merkle state trie (a
//! `StandardBlock`'s `MerkleRoot()` is always the zero id), so the reexecute
//! "root" the oracle compares is the deterministic POST-STATE DIGEST: a `sha256`
//! over the canonically-sorted final UTXO set, alongside the chain-tip block id +
//! height. Two replays of the same case produce byte-identical roots — the
//! determinism / reproducibility property the recorded-oracle path proves WITHOUT
//! a live Go oracle (mirroring the `ava-differential` `xchain` collector). The Go
//! recorded-oracle parity arm is the follow-up (see `tests/PORTING.md`).
//!
//! The seed-derived program + VM-driving flow are ported from the
//! `ava-differential` `xchain` collector (M5.22). The key difference: this lib
//! code propagates VM/codec errors via [`crate::Error`] instead of `expect`-ing
//! them (no `unwrap`/`expect` in `ava-reexecute` lib code), and it captures a
//! reexecute-shaped root struct rather than a normalized differential observation.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use ava_avm::config::Config;
use ava_avm::state::{Chain, ReadOnlyChain, State};
use ava_avm::txs::codec::Codec;
use ava_avm::txs::components::{AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput};
use ava_avm::txs::executor::semantic::Utxo;
use ava_avm::txs::{BaseTx, CreateAssetTx, FxCredential, InitialState, Tx, UnsignedTx};
use ava_avm::vm::{AvmVm, DynDb};
use ava_crypto::hashing;
use ava_database::{DynDatabase, MemDb};
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;

use crate::{Error, Result};

/// The deterministic roots a single X-Chain reexecute case produces.
///
/// `last_accepted_id` is the chain-tip block id after replaying the case (the
/// X-Chain analogue of a state/merkle root the reexecute oracle keys on);
/// `state_digest` is the `sha256` over the canonically-sorted final UTXO set (the
/// post-state digest, since the X-Chain keeps no merkle trie). Two replays of the
/// same case must produce an identical [`XchainReexecuteRoots`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XchainReexecuteRoots {
    /// The chain-tip (last-accepted) block id after the replay.
    pub last_accepted_id: [u8; 32],
    /// The chain-tip block height after the replay (genesis is `0`).
    pub last_accepted_height: u64,
    /// `sha256` over the canonically-sorted `(utxo_id ++ utxo_bytes)` of the final
    /// UTXO set — the deterministic post-state digest.
    pub state_digest: [u8; 32],
}

// ---------------------------------------------------------------------------
// Fixed chain identity (the synthetic-genesis seeding the VM conformance battery
// uses; ported from the `ava-differential` `xchain` collector — M5.22).
// ---------------------------------------------------------------------------

const NETWORK_ID: u32 = 10;
/// The (arbitrary) stop-vertex id the genesis block parents off (specs 09 §1).
const STOP_VERTEX: [u8; 32] = [0x07; 32];
/// The genesis Unix timestamp encoded into the synthetic genesis bytes.
///
/// Deliberately FAR in the future (year ~2255) so every built block inherits the
/// fixed `parent_time` (the X-Chain builder stamps `time = max(parent_time, now)`
/// and the engine feeds it wall-clock `now`); a future genesis pins the block ids
/// deterministically across runs. (Same clock-pinning the `ava-differential`
/// `xchain` collector uses until `build_block` adopts an injectable clock.)
const GENESIS_TS: u64 = 9_000_000_000;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

/// A fixed `CreateAssetTx` establishing the asset the seeded UTXOs belong to. The
/// semantic verifier loads it from the tx store to check fx usage, so it enables
/// fx index 0 (secp). Its id is the asset id every UTXO/tx in the case references.
fn create_asset_tx() -> CodecCase<Tx> {
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
    tx.initialize(Codec())
        .map_err(|e| Error::Xchain(format!("initialize create-asset: {e}")))?;
    Ok(tx)
}

/// Result alias for the fallible builder helpers below (avoids `expect` in lib
/// code). Named for readability at the call sites.
type CodecCase<T> = Result<T>;

fn asset_id() -> Result<Id> {
    Ok(create_asset_tx()?.id())
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

// ---------------------------------------------------------------------------
// The seed-derived program (BaseTx chain).
// ---------------------------------------------------------------------------

/// One seed-derived issuance in the synthetic case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BaseTransfer {
    /// The (full) amount transferred — produced UTXO chains into the next spend.
    amount: u64,
}

/// Derives a small, bounded program of chained `BaseTx` spends entirely from
/// `seed`, so the produced txs (and thus roots) are byte-identical across runs.
fn program(seed: u64) -> Vec<BaseTransfer> {
    // Chain length 1..=4: `& 3` keeps it in 0..=3, `saturating_add(1)` lifts it.
    let count = (mix(seed) & 3).saturating_add(1);
    let mut specs = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for _ in 0..count {
        // A fixed seed-derived amount keeps every full-amount spend balanced
        // (zero fee), so each produced UTXO funds the next spend exactly.
        let amount = (mix(seed.wrapping_add(0xA5A5)) % 9_000).saturating_add(1_000);
        specs.push(BaseTransfer { amount });
    }
    specs
}

/// A tiny deterministic bit-mixer (splitmix64 finalizer) — pure, no global state.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// Deterministic tx construction.
// ---------------------------------------------------------------------------

/// The id of the synthetic tx that produced the seeded genesis UTXO `U0`. Fully
/// seed-derived so two runs seed an identical UTXO.
fn genesis_utxo_tx_id(seed: u64) -> Id {
    Id::from(to_be_bytes_id(mix(seed)))
}

/// The codec bytes (and input id) of a secp transfer UTXO produced by `tx_id` at
/// `output_index` holding `amount` of the asset.
fn utxo_bytes(tx_id: Id, output_index: u32, amount: u64) -> Result<(Id, Vec<u8>)> {
    let utxo = Utxo {
        tx_id,
        output_index,
        asset_id: asset_id()?,
        out: Output::SecpTransfer(TransferOutput::new(amount, owners())),
    };
    let bytes = utxo
        .marshal()
        .map_err(|e| Error::Xchain(format!("marshal seeded utxo: {e}")))?;
    Ok((utxo.input_id(), bytes))
}

/// A signed, initialized `BaseTx` consuming `(in_tx_id, 0)` holding `amount` and
/// producing a single output of the same `amount` at its own index 0 (zero fee),
/// so its produced UTXO chains into the next spend. Byte-identical for a given
/// `(in_tx_id, amount)`.
fn base_tx(in_tx_id: Id, amount: u64) -> Result<Tx> {
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![TransferableOutput {
            asset_id: asset_id()?,
            out: Output::SecpTransfer(TransferOutput::new(amount, owners())),
        }],
        ins: vec![TransferableInput {
            tx_id: in_tx_id,
            output_index: 0,
            asset_id: asset_id()?,
            r#in: Input::SecpTransfer(TransferInput::new(amount, vec![0])),
        }],
        memo: Vec::new(),
    })));
    // A fixed (empty-sig) credential — the verifier runs un-bootstrapped here, so
    // signatures are not checked (matching the M5.19 conformance seeding).
    tx.creds = vec![FxCredential::new(
        Id::EMPTY,
        SecpCredential::new(vec![[0u8; 65]]),
    )];
    tx.initialize(Codec())
        .map_err(|e| Error::Xchain(format!("initialize base tx: {e}")))?;
    Ok(tx)
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
        chain_id: chain_id(),
        node_id: NodeId::default(),
        public_key: None,
        network_upgrades: ava_version::upgrade::get_config(1),
        x_chain_id: chain_id(),
        c_chain_id: Id::EMPTY,
        avax_asset_id: Id::EMPTY,
        chain_data_dir: std::path::PathBuf::new(),
    })
}

/// The minimal synthetic X-Chain genesis seed: the 32-byte stop-vertex id followed
/// by the 8-byte big-endian Unix-second timestamp (the M5.19 shape).
fn genesis_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&STOP_VERTEX);
    out.extend_from_slice(&GENESIS_TS.to_be_bytes());
    out
}

/// Replay a synthetic seed-derived X-Chain reexecute case through the REAL
/// `ava-avm` VM/block pipeline and return its deterministic roots.
///
/// Seeds one spendable genesis UTXO `U0`, then for each derived `BaseTransfer`
/// builds a `BaseTx` spending the previous output, admits it, and drives
/// `build → set_preference → verify → accept` so each tx lands in its own accepted
/// block. The returned [`XchainReexecuteRoots`] carries the chain-tip block id +
/// height and the `sha256` post-state digest over the sorted final UTXO set.
///
/// # Errors
/// Returns an [`Error::Xchain`] if any VM/codec step fails (initialize, seed,
/// admit, build, verify, accept, or the post-state read).
pub fn replay_xchain(seed: u64) -> Result<XchainReexecuteRoots> {
    // One multi-thread runtime per call keeps each VM instance fully independent
    // (the reexecute determinism gate replays the same case twice and compares).
    let rt = Runtime::new().map_err(|e| Error::Xchain(format!("tokio runtime: {e}")))?;
    rt.block_on(replay_xchain_async(seed))
}

async fn replay_xchain_async(seed: u64) -> Result<XchainReexecuteRoots> {
    let token = CancellationToken::new();
    let mut vm = AvmVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    // A zero-fee config keeps each full-amount transfer balanced without a fee
    // UTXO (the M5.19 conformance seeding).
    let config_bytes = serde_json::to_vec(&Config {
        tx_fee: 0,
        create_asset_tx_fee: 0,
    })
    .map_err(|e| Error::Xchain(format!("config bytes: {e}")))?;

    vm.initialize(
        &token,
        chain_ctx(),
        db,
        &genesis_bytes(),
        b"",
        &config_bytes,
        Vec::new(),
        Arc::new(NoopAppSender),
    )
    .await
    .map_err(|e| Error::Xchain(format!("initialize: {e}")))?;

    // Seed the single spendable genesis UTXO U0 (the genesis-asset alloc is the
    // M8/`ava-genesis` follow-up — seeded directly here).
    let specs = program(seed);
    let first_amount = specs.first().map(|s| s.amount).unwrap_or(0);
    let u0_tx_id = genesis_utxo_tx_id(seed);
    let (u0_id, u0_bytes) = utxo_bytes(u0_tx_id, 0, first_amount)?;
    let ca = create_asset_tx()?;
    let ca_id = ca.id();
    let ca_bytes = ca.bytes().to_vec();
    vm.seed_genesis_state(|s: &mut State<DynDb>| {
        // The asset's CreateAssetTx (so the semantic verifier's `verify_fx_usage`
        // resolves fx index 0 = secp) + the single spendable genesis UTXO U0.
        s.add_tx(ca_id, ca_bytes);
        s.add_utxo(u0_id, u0_bytes);
    })
    .map_err(|e| Error::Xchain(format!("seed genesis state: {e}")))?;

    // Track every UTXO id the case touches so the post-state read can reach each
    // back (no enumeration API on `Chain` — M5.10 stores UTXOs keyed by id).
    let mut touched: Vec<Id> = vec![u0_id];

    // Build the chain of spends: each tx consumes the previous output (U0 for the
    // first) and produces a single output at its own index 0.
    let mut prev_tx_id = u0_tx_id;
    for spec in &specs {
        let tx = base_tx(prev_tx_id, spec.amount)?;
        let this_tx_id = tx.id();
        let (produced_id, _) = utxo_bytes(this_tx_id, 0, spec.amount)?;
        touched.push(produced_id);

        vm.mempool_add(tx)
            .map_err(|e| Error::Xchain(format!("mempool add: {e}")))?;

        // Build → set_preference → verify → accept this tx's block, advancing
        // last-accepted by one height. The builder packs the single pending tx.
        let blk = vm
            .build_block(&token)
            .await
            .map_err(|e| Error::Xchain(format!("build_block: {e}")))?;
        let blk_id = blk.id();
        vm.set_preference(&token, blk_id)
            .await
            .map_err(|e| Error::Xchain(format!("set_preference: {e}")))?;
        blk.verify(&token)
            .await
            .map_err(|e| Error::Xchain(format!("verify: {e}")))?;
        blk.accept(&token)
            .await
            .map_err(|e| Error::Xchain(format!("accept: {e}")))?;

        prev_tx_id = this_tx_id;
    }

    // Capture the chain-tip block id + height.
    let last_id = vm
        .last_accepted(&token)
        .await
        .map_err(|e| Error::Xchain(format!("last_accepted: {e}")))?;
    let last_block = vm
        .get_block(&token, last_id)
        .await
        .map_err(|e| Error::Xchain(format!("get_block: {e}")))?;
    let last_accepted_height = last_block.height();

    // Read back the final UTXO set: every touched id that still resolves (consumed
    // UTXOs are deleted on accept, so they drop out — capturing the spend
    // transition). The `Chain` trait exposes no enumeration (M5.10 keys UTXOs by
    // id), so we read back exactly the ids the case touched via the read-only seam.
    let mut survivors: Vec<(Id, Vec<u8>)> = vm
        .with_state(|state: &State<DynDb>| {
            let mut out = Vec::new();
            for id in &touched {
                if let Ok(bytes) = state.get_utxo(*id) {
                    out.push((*id, bytes));
                }
            }
            out
        })
        .map_err(|e| Error::Xchain(format!("read utxo set: {e}")))?;

    vm.shutdown(&token)
        .await
        .map_err(|e| Error::Xchain(format!("shutdown: {e}")))?;

    // The post-state digest: sha256 over the canonically-sorted (by id)
    // `(utxo_id ++ utxo_bytes)` concatenation. Sorting by the 32-byte id makes the
    // digest independent of read-back order (no HashMap iteration leak, specs/00
    // §6.1).
    survivors.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut preimage = Vec::new();
    for (id, bytes) in &survivors {
        preimage.extend_from_slice(id.as_bytes());
        preimage.extend_from_slice(bytes);
    }
    let state_digest = hashing::sha256(&preimage);

    let mut last_accepted_id = [0u8; 32];
    last_accepted_id.copy_from_slice(last_id.as_bytes());

    Ok(XchainReexecuteRoots {
        last_accepted_id,
        last_accepted_height,
        state_digest,
    })
}

/// A `u64` → 32-byte id (first 8 bytes big-endian, rest zero).
fn to_be_bytes_id(x: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&x.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_is_seed_deterministic() {
        assert_eq!(program(7), program(7));
        assert_eq!(program(0xDEAD_BEEF), program(0xDEAD_BEEF));
    }

    #[test]
    fn replay_xchain_is_deterministic() {
        let a = replay_xchain(42).expect("first replay");
        let b = replay_xchain(42).expect("second replay");
        assert_eq!(a, b, "same case must produce identical roots");
        assert!(a.last_accepted_height >= 1, "expected an accepted block");
        assert_ne!(a.state_digest, [0u8; 32], "real post-state digest");
    }

    #[test]
    fn perturbing_seed_changes_roots() {
        let a = replay_xchain(1).expect("replay seed 1");
        let b = replay_xchain(2).expect("replay seed 2");
        assert_ne!(a, b, "different cases should produce different roots");
    }
}
