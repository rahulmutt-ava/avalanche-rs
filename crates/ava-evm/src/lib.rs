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

// On-demand block builder driver (G5, §4) — M6.20.
pub mod builder;

// ChainVm adapter (§3) — M6.10.
pub mod vm;

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
