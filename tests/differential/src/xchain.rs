// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) tx-issuance differential seam (M5.22, specs 09 §1/§7, 02 §11,
//! 00 §6.1).
//!
//! This module contributes the X-Chain's entry into the per-subsystem
//! [`Observation`]-collector model (the X.13 spec: "each subsystem adds its
//! collector — M5 X-Chain"):
//!
//! * a deterministic, seed-driven generator that builds X-Chain `BaseTx`
//!   issuances whose tx/key bytes are FULLY seed-derived (so two runs with the
//!   same seed produce byte-identical txs), and
//! * a normalized [`Observation`] collector + [`run_program`] that drives a fresh
//!   `ava-avm` VM through the REAL block pipeline (seed genesis state → admit txs
//!   → build → verify → accept) and captures the final state.
//!
//! ## What the per-PR gate proves (today)
//!
//! `differential::xchain_issue_tx` (the proptest in `tests/xchain_issue_tx.rs`)
//! runs [`run_program`] twice on two INDEPENDENT VM instances for the same seed
//! and asserts the two normalized [`Observation`]s are byte-identical — the
//! determinism / total-order property (specs 00 §6.1, 02 §11). This is the
//! meaningful property available now: there is no Go recorded-oracle and no live
//! two-binary mode yet.
//!
//! ## SCAFFOLD / deferred (the Go-oracle arms — X.13/X.15)
//!
//! The live two-binary + Go recorded-oracle `differential::xchain_issue_tx` arms
//! are gated behind the (unimplemented) [`LockstepDriver`](crate::LockstepDriver)
//! — its `replay_recorded` is owned by tier-X task X.13 and there is no live mode
//! yet (matching the M5.20 `atomic` collector + the harness scaffold). When the
//! live mode lands, the same [`Observation`] shape this module emits is compared
//! between the Go and Rust nodes after the same seed-derived program.
//
// TODO(X.13/X.15): (1) richer tx kinds — CreateAsset / Operation / Import /
// Export (the generator is structured around `TxSpec` so they slot in as new
// variants); (2) the Go recorded-oracle + live two-binary comparison; (3) scale
// the proptest cases to 10k. See `tests/xchain_issue_tx.rs` for the gate.

// This module is test-harness support: it drives a known-good deterministic
// program through the real VM, so a VM/codec error is a genuine regression that
// SHOULD panic loudly (there is no recovery path for the differential gate). The
// `expect`/`unwrap` calls below are all on that known-good path, mirroring the
// M5.19 `vm_conformance.rs` seeding helpers (which carry the same allow).
#![allow(clippy::expect_used, clippy::unwrap_used)]

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
use ava_database::{DynDatabase, MemDb};
use ava_secp256k1fx::{Credential as SecpCredential, OutputOwners, TransferInput, TransferOutput};
use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;
use ava_vm::app_sender::{AppSender, SendConfig};
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;

use crate::observation::Observation;

// ---------------------------------------------------------------------------
// Fixed chain identity (the synthetic-genesis seeding the VM conformance battery
// uses; the full Go-format X-Chain genesis-asset alloc is the M8/`ava-genesis`
// follow-up).
// ---------------------------------------------------------------------------

const NETWORK_ID: u32 = 10;
/// The (arbitrary) stop-vertex id the genesis block parents off (specs 09 §1).
const STOP_VERTEX: [u8; 32] = [0x07; 32];
/// The genesis Unix timestamp encoded into the synthetic genesis bytes.
///
/// Deliberately set FAR in the future (year ~2255). The X-Chain block builder
/// stamps each block with `time = max(parent_time, now)` (Unix seconds), and the
/// engine-facing `ChainVm::build_block` feeds it `now = SystemTime::now()` — a
/// wall-clock value the harness cannot inject. With a genesis time that always
/// exceeds `now`, every built block inherits `parent_time` (the fixed
/// genesis-derived value) deterministically, so the block ids are reproducible
/// across runs. Were genesis in the past, `now` would win and the (second-
/// truncated) wall clock would occasionally straddle a second boundary between
/// the two determinism-gate runs, making the block id nondeterministic. This is
/// the harness's clock-pinning until `ava-avm` `build_block` adopts the
/// injectable `ava_utils::clock::Clock` seam (tracked by tier-X task X.19).
const GENESIS_TS: u64 = 9_000_000_000;

fn chain_id() -> Id {
    Id::from([0x05; 32])
}

/// A fixed (NOT seed-derived) `CreateAssetTx` that establishes the asset the
/// seeded UTXOs belong to. The semantic verifier loads this from the tx store to
/// check fx usage (`verify_fx_usage`), so it must enable fx index 0 (secp). Its
/// id is the asset id every UTXO/tx in the program references. (The full Go-format
/// genesis-asset alloc is the M8/`ava-genesis` follow-up — seeded directly here.)
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

fn asset_id() -> Id {
    create_asset_tx().id()
}

fn addr() -> ShortId {
    ShortId::from([0xab; 20])
}

fn owners() -> OutputOwners {
    OutputOwners::new(0, 1, vec![addr()])
}

// ---------------------------------------------------------------------------
// The seed-derived program (BaseTx only; extension points for the richer kinds).
// ---------------------------------------------------------------------------

/// One seed-derived issuance the generator emits.
///
/// SCAFFOLD: only [`TxSpec::BaseTransfer`] is wired today (the plan's TDD entry
/// point). CreateAsset / Operation / Import / Export slot in here as new variants
/// (see the module-level `TODO(X.13/X.15)`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TxSpec {
    /// Spend the UTXO produced by the previous tx (or the seeded genesis UTXO for
    /// the first), transferring the full `amount` to a single fresh output at
    /// index 0 — so the produced UTXO chains into the next spend (zero fee).
    BaseTransfer { amount: u64 },
}

/// Derives a small, bounded program of [`TxSpec`]s entirely from `seed`, so the
/// produced txs are byte-identical across runs.
///
/// The count (1..=4) and each transfer amount are derived from the seed via a
/// tiny deterministic splitmix-style mix — no RNG state escapes this function.
fn program(seed: u64) -> Vec<TxSpec> {
    // Number of chained BaseTx spends: 1..=4 (the genesis UTXO funds the chain;
    // every spend transfers the FULL amount, so the chain length is bounded only
    // by the derivation, not by the balance). `& 3` keeps it in 0..=3 (a 2-bit
    // mask, no overflow), then `saturating_add(1)` lifts it to 1..=4.
    let count = (mix(seed) & 3).saturating_add(1);
    let mut specs = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for _ in 0..count {
        // A fixed transfer amount keeps every spend balanced (full-amount,
        // zero-fee). The amount is seed-derived but identical across the chain so
        // each produced UTXO funds the next spend exactly. `% 9_000` (u64, never
        // overflows) then `saturating_add(1_000)` gives 1_000..=9_999.
        let amount = (mix(seed.wrapping_add(0xA5A5)) % 9_000).saturating_add(1_000);
        specs.push(TxSpec::BaseTransfer { amount });
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
    Id::from(mix(seed).to_be_bytes_id())
}

/// The codec bytes (and input id) of a secp transfer UTXO produced by `tx_id` at
/// `output_index` holding `amount` of [`asset_id`].
fn utxo_bytes(tx_id: Id, output_index: u32, amount: u64) -> (Id, Vec<u8>) {
    let utxo = Utxo {
        tx_id,
        output_index,
        asset_id: asset_id(),
        out: Output::SecpTransfer(TransferOutput::new(amount, owners())),
    };
    (
        utxo.input_id(),
        utxo.marshal().expect("marshal seeded utxo"),
    )
}

/// A signed, initialized `BaseTx` consuming `(in_tx_id, 0)` holding `amount` and
/// producing a single output of the same `amount` at its own index 0 (zero fee),
/// so its produced UTXO chains into the next spend. Byte-identical for a given
/// `(in_tx_id, amount)`.
fn base_tx(in_tx_id: Id, amount: u64) -> Tx {
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::new(AvaxBaseTx {
        network_id: NETWORK_ID,
        blockchain_id: chain_id(),
        outs: vec![TransferableOutput {
            asset_id: asset_id(),
            out: Output::SecpTransfer(TransferOutput::new(amount, owners())),
        }],
        ins: vec![TransferableInput {
            tx_id: in_tx_id,
            output_index: 0,
            asset_id: asset_id(),
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
    tx.initialize(Codec()).expect("initialize base tx");
    tx
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

/// The minimal synthetic X-Chain genesis seed: the 32-byte stop-vertex id
/// followed by the 8-byte big-endian Unix-second timestamp (the M5.19 shape).
fn genesis_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&STOP_VERTEX);
    out.extend_from_slice(&GENESIS_TS.to_be_bytes());
    out
}

/// Runs the seed-derived program through a fresh `ava-avm` VM and returns the
/// normalized final [`Observation`].
///
/// Seeds one spendable genesis UTXO `U0`, then for each [`TxSpec`] in the program
/// builds a `BaseTx` spending the previous output, admits it, and drives
/// `build → verify → accept` so each tx lands in its own accepted block. The
/// observation records the last-accepted block id + height and the full sorted
/// UTXO set (every UTXO the program touched, surviving ones only).
///
/// # Panics
/// Panics (via `expect`) on any VM/codec error — this is test-only harness code
/// driving a known-good deterministic program; a failure is a real regression.
#[must_use]
pub fn run_program(seed: u64) -> Observation {
    // One multi-thread runtime per call keeps each VM instance fully independent
    // (the determinism gate runs `run_program` twice and compares); a shared
    // runtime would still be correct, but per-call isolation is the cleaner model
    // for the self-vs-self property. (Reusing ONE runtime across cases is the
    // documented option if this ever proves slow.)
    let rt = Runtime::new().expect("tokio runtime");
    rt.block_on(run_program_async(seed))
}

async fn run_program_async(seed: u64) -> Observation {
    let token = CancellationToken::new();
    let mut vm = AvmVm::new();
    let db: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    // A zero-fee config keeps each full-amount transfer balanced without a fee
    // UTXO (the M5.19 conformance seeding).
    let config_bytes = serde_json::to_vec(&Config {
        tx_fee: 0,
        create_asset_tx_fee: 0,
    })
    .expect("config bytes");

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
    .expect("initialize");

    // Seed the single spendable genesis UTXO U0 (the genesis-asset alloc is the
    // M8/`ava-genesis` follow-up — seeded directly here).
    let specs = program(seed);
    let first_amount = match specs.first() {
        Some(TxSpec::BaseTransfer { amount }) => *amount,
        None => 0,
    };
    let u0_tx_id = genesis_utxo_tx_id(seed);
    let (u0_id, u0_bytes) = utxo_bytes(u0_tx_id, 0, first_amount);
    let ca = create_asset_tx();
    let ca_id = ca.id();
    let ca_bytes = ca.bytes().to_vec();
    vm.seed_genesis_state(|s: &mut State<DynDb>| {
        // The asset's CreateAssetTx (so the semantic verifier's `verify_fx_usage`
        // resolves fx index 0 = secp) + the single spendable genesis UTXO U0.
        s.add_tx(ca_id, ca_bytes);
        s.add_utxo(u0_id, u0_bytes);
    })
    .expect("seed genesis state");

    // Track every UTXO id the program touches so the observation can read each
    // back (no enumeration API on `Chain` — M5.10 stores UTXOs keyed by id).
    let mut touched: Vec<Id> = vec![u0_id];

    // Build the chain of spends: each tx consumes the previous output (U0 for the
    // first) and produces a single output at its own index 0.
    let mut prev_tx_id = u0_tx_id;
    for spec in &specs {
        let TxSpec::BaseTransfer { amount } = spec;
        let tx = base_tx(prev_tx_id, *amount);
        let this_tx_id = tx.id();
        // The produced UTXO id (this tx's output 0) — track it for the observation.
        let (produced_id, _) = utxo_bytes(this_tx_id, 0, *amount);
        touched.push(produced_id);

        vm.mempool_add(tx).expect("mempool add");

        // Build → set_preference → verify → accept this tx's block, advancing
        // last-accepted by one height. The builder packs the single pending tx.
        let blk = vm.build_block(&token).await.expect("build_block");
        let blk_id = blk.id();
        vm.set_preference(&token, blk_id)
            .await
            .expect("set_preference");
        blk.verify(&token).await.expect("verify");
        blk.accept(&token).await.expect("accept");

        prev_tx_id = this_tx_id;
    }

    // Collect the observation.
    let last_id = vm.last_accepted(&token).await.expect("last_accepted");
    let height = block_height(&vm, &token, last_id).await;

    let mut fields = vec![
        (
            "xchain.last_accepted.id".to_owned(),
            hex_lower(last_id.as_bytes()),
        ),
        ("xchain.last_accepted.height".to_owned(), height.to_string()),
    ];

    // The full UTXO set: every touched id that still resolves (consumed UTXOs are
    // deleted on accept, so they drop out — capturing the spend transition). The
    // `Chain` trait exposes no enumeration (M5.10 keys UTXOs by id), so we read
    // back exactly the ids the program touched via the read-only state seam.
    vm.with_state(|state: &State<DynDb>| {
        for id in &touched {
            if let Ok(bytes) = state.get_utxo(*id) {
                fields.push((
                    format!("xchain.utxo.{}", hex_lower(id.as_bytes())),
                    hex_lower(&bytes),
                ));
            }
        }
    })
    .expect("read utxo set");

    vm.shutdown(&token).await.expect("shutdown");

    Observation { fields }.normalized()
}

/// Reads the height of accepted block `id` via the engine-facing `get_block`.
async fn block_height(vm: &AvmVm, token: &CancellationToken, id: Id) -> u64 {
    vm.get_block(token, id)
        .await
        .expect("get last-accepted block")
        .height()
}

/// Lower-hex encode without pulling a dependency into the harness scaffold
/// (matching the `atomic` collector's `hex_lower`).
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

/// Extension helper: a `u64` → 32-byte id (first 8 bytes big-endian, rest zero).
trait ToBeBytesId {
    fn to_be_bytes_id(self) -> [u8; 32];
}

impl ToBeBytesId for u64 {
    fn to_be_bytes_id(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[..8].copy_from_slice(&self.to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_is_seed_deterministic() {
        // Same seed → identical program; different seed → (very likely) different.
        assert_eq!(program(7), program(7));
        assert_eq!(program(0xDEAD_BEEF), program(0xDEAD_BEEF));
    }

    #[test]
    fn run_program_is_deterministic() {
        let a = run_program(42);
        let b = run_program(42);
        assert_eq!(a, b, "same seed must produce identical observations");
        // Non-trivial: a block was accepted (height >= 1) and the UTXO set is
        // non-empty (the final produced output survives).
        let height = a
            .fields
            .iter()
            .find(|(k, _)| k == "xchain.last_accepted.height")
            .map(|(_, v)| v.parse::<u64>().expect("height parses"))
            .expect("height field");
        assert!(
            height >= 1,
            "expected an accepted block, got height {height}"
        );
        assert!(
            a.fields.iter().any(|(k, _)| k.starts_with("xchain.utxo.")),
            "expected at least one surviving UTXO in the observation"
        );
    }

    #[test]
    fn perturbing_seed_changes_observation() {
        // Sanity-check the determinism assertion genuinely catches divergence:
        // two DIFFERENT seeds must (with overwhelming probability) differ.
        let a = run_program(1);
        let b = run_program(2);
        assert_ne!(
            a, b,
            "different seeds should produce different observations"
        );
    }
}
