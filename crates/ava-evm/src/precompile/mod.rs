// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Stateful precompiles + registry: warp, allowlist, feemanager, nativeminter,
//! rewardmanager (G4, spec 10 §8, spec 20 §7).
//!
//! M6.21 delivered the **registry, provider, height-gating, and fall-through**
//! plumbing: the [`registry::PrecompileRegistry`] (address to stateful precompile
//! plus activation timestamp), the [`registry::AvaPrecompiles`] revm
//! [`PrecompileProvider`] that overlays the activated Avalanche precompiles on
//! revm's standard Ethereum set, and the [`registry::AvaCtxExt`] revm context
//! extension (G10) whose `predicates` field the pre-execution predicate pass
//! fills with verified warp results.
//!
//! M6.22 lands the [`warp`] precompile body + the **predicate pass**
//! ([`warp::run_predicates`]) that verifies warp BLS aggregates against the
//! source-subnet validator set at the proposervm-pinned P-Chain height and
//! stashes a `Vec<bool>` into [`registry::PredicateResults`] (G4, spec 20 §7).
//!
//! **Deferred (→ M6.31):** the live `EvmFactory` that installs
//! [`registry::AvaPrecompiles`] + [`registry::AvaCtxExt`] onto the revm context's
//! `Chain` slot during `execute_batch` (the bare-executor path churn M6.21
//! flagged), and the other ConfigKey precompile bodies
//! (AllowList/FeeManager/NativeMinter/RewardManager/GasPriceManager). They
//! register as [`registry::StatefulPrecompile`]s exactly like [`warp`] once
//! ported.
//!
//! [`PrecompileProvider`]: ava_evm_reth::PrecompileProvider

pub mod registry;
pub mod warp;
