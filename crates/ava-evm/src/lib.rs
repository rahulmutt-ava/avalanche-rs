// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-evm` — the Avalanche **C-Chain** VM (port of coreth `plugin/evm` +
//! `atomic` + `customheader`), built on **reth/revm as a *library* executor**.
//!
//! Tier T4 (VMs). Owning spec: `specs/10-cchain-evm-reth.md` (PRIMARY), plus
//! `04` §4 (Firewood-ethhash state-of-record), `20` §7 (warp precompile),
//! `21` (per-fork dynamic fees). Integration mode is **reth-as-a-library, NOT
//! the Engine API** (spec 00 §11.1.6): Snowman owns fork choice; we drive the
//! bare `BlockExecutor`/`BlockBuilder` to get a pre-commit state root to vote
//! on, and Accept/Reject map to Firewood `commit`/discard with no reorgs.
//!
//! Every reth/revm/alloy touch-point goes through the [`ava_evm_reth`] facade
//! (G0); this crate never names `reth_*` directly. The module tree mirrors the
//! Go layout (spec 10 §13) and is populated tier-by-tier across the M6 wave
//! plan (see `plan/M6-cchain.md`).

#![forbid(unsafe_code)]

pub mod error;

// State backend over Firewood-ethhash (G1, §5) — M6.3/M6.4.
pub mod state;

// Fork schedule + chain spec (G7, §7.4/§11) — M6.5/M6.8.
pub mod chainspec;

// Per-fork dynamic fee rules: AP3 window, AP4 block gas cost, Fortuna/ACP-176,
// ACP-226 (G2, §7) — M6.11/M6.12/M6.13.
pub mod feerules;

// EVM configuration + external-consensus executor driving (§7/§8) — M6.6.
pub mod evmconfig;

// Block wire format + EvmBlock lifecycle (§3/§9.3) — M6.7/M6.9.
pub mod block;

// Canonical (non-state) MDBX store: headers/bodies/receipts (G6, §3) — M6.9.
pub mod canonical;

// Accepted-tx receipts: verify-time stash, accept-time persisted encoding +
// `AcceptedTxIndex` (cchain-tx-pipeline design doc, task 3).
pub mod receipts;

// On-demand block builder driver (G5, §4) — M6.20.
pub mod builder;

// ChainVm adapter (§3) — M6.10.
pub mod vm;

// EVM mempool: coreth-parity admission validation, storage, eviction
// (cchain-tx-pipeline design doc 2026-07-17, task 1).
pub mod mempool;

// C-Chain tx gossip: GossipEthTx + marshaller + bloom-backed Set over
// EvmMempool (cchain-tx-gossip design doc, task 11).
pub mod gossip;

// Atomic X<->C txs: types/codec, mempool, backend, atomic trie, state hook,
// semantic verify (G3, §6) — M6.14..M6.18.
pub mod atomic;

// Stateful precompiles + registry + warp/allowlist/feemanager/... (G4, §8) —
// M6.21/M6.22.
pub mod precompile;

// eth_* + avax.* RPC over Firewood (G8, §9) — M6.23/M6.24.
pub mod rpc;

// EVM + atomic-trie state sync over Firewood proofs (G8, §10) — M6.25.
pub mod sync;

pub use error::{Error, Result};
// Spec 21 §0 gas primitives, re-exported at the crate root for ergonomic use by
// fee-schedule callers/tests (the canonical owner is `ava_vm::components::gas`,
// re-exported through `feerules`).
pub use feerules::{Gas, GasState, Price};

// =========================================================================
// Reusable API surface for SAE — the §16 / §17.10 reuse contract
// =========================================================================
//
// **"One EVM engine, two drivers" (spec 10 §16, §17.10; 00 §11.1.5).** The
// items below are the *stable, flat* public surface SAE's `ava-saevm-exec`
// (spec 11 §6) drives the EVM engine through. They are re-exported at the crate
// ROOT (not only under their owning submodules) so the SAE driver depends on a
// minimal, stable path set — never on `EvmVm`, `BlockBuilderDriver`, or reth
// directly. Each maps to a §17.10 table row:
//
// | §17.10 item                                  | re-exported below as                |
// |----------------------------------------------|-------------------------------------|
// | `AvaEvmConfig` (+ `execute_batch`)           | [`AvaEvmConfig`]                    |
// | `FirewoodStateProvider`/`…View`/`…Committer` | [`FirewoodStateProvider`] / [`FirewoodStateView`] (the "committer" is the provider's deferred propose/commit handles — see below) |
// | `hashed_post_state_to_batchops`              | [`hashed_post_state_to_batchops`]   |
// | `propose_from_bundle` / view-by-root         | [`FirewoodStateProvider::propose_from_bundle`] / [`FirewoodStateProvider::history_by_state_root`] |
// | `AvaPrecompiles` / `PrecompileRegistry`      | [`AvaPrecompiles`] / [`PrecompileRegistry`] |
// | `AtomicStateHook`                            | [`AtomicStateHook`]                 |
// | `AvaChainSpec` / `revm_spec_id`              | [`AvaChainSpec`] (+ `AvaChainSpec::revm_spec_id`) |
//
// The facade twins (`ExternalConsensusExecutor`, `ExecOutcome`, `AvaEvmEnv`,
// `RecoveredTx`, `PreExecutionHook`, the revm `State`/`StateBuilder`/
// `StateProviderDatabase` overlay types) live in [`ava_evm_reth`] — SAE imports
// those from the facade, never reth itself (G0).
//
// **NOTE on `FirewoodStateCommitter` (§17.10 table name).** There is no separate
// `FirewoodStateCommitter` type as-built: the "open view by root → propose →
// defer-commit-on-interval" capability the table attributes to it lives as
// methods on [`FirewoodStateProvider`]
// ([`propose_from_bundle`](FirewoodStateProvider::propose_from_bundle),
// [`propose_and_stash`](FirewoodStateProvider::propose_and_stash),
// [`stash_proposal`](FirewoodStateProvider::stash_proposal),
// [`commit`](FirewoodStateProvider::commit),
// [`discard`](FirewoodStateProvider::discard)). A single owner of both the read
// view and the commit stash keeps the propose/commit keyed by post-state root in
// one place; SAE's `Tracker` holds an `Arc<FirewoodStateProvider>` and gets the
// "committer" role from those methods.
//
// **NOT part of this surface — the block lifecycle (spec 10 §17.10):**
// [`vm::EvmVm`]/[`block::EvmBlock`] (the synchronous `ChainVm`/verify-then-vote
// driver, §3) and [`builder::BlockBuilderDriver`] (§17.6) are
// **sync-C-Chain-only**. SAE supplies its *own* streaming lifecycle
// (order→execute→settle, spec 11 §6) but drives the *same*
// [`AvaEvmConfig::execute_batch`] + Firewood propose/commit underneath. The
// reuse contract is *enforced* by `tests/reuse_surface.rs`, which drives a batch
// end-to-end using only the items below + the facade, never naming `EvmVm` /
// `EvmBlock` / `BlockBuilderDriver` / reth.

#[doc(inline)]
pub use atomic::hook::AtomicStateHook;
#[doc(inline)]
pub use chainspec::AvaChainSpec;
#[doc(inline)]
pub use evmconfig::{AvaEvmConfig, AvaState, NoopPreHook};
#[doc(inline)]
pub use precompile::registry::{AvaPrecompiles, PrecompileRegistry};
#[doc(inline)]
pub use state::{FirewoodStateProvider, FirewoodStateView, hashed_post_state_to_batchops};
