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
//! M6.31 lands the live `crate::evmconfig::AvaEvmFactory` that installs the
//! registered modules into every EVM's `PrecompilesMap` during `execute_batch`
//! (height-gated, predicate results threaded per tx index), plus the ConfigKey
//! precompile bodies: [`allowlist`] (shared role machinery + the standalone
//! Deployer/Tx allow lists), [`nativeminter`], [`feemanager`],
//! [`rewardmanager`], and [`gaspricemanager`] — each a
//! [`registry::StatefulPrecompile`] ported byte-exact from subnet-evm
//! `precompile/contracts/*` (golden vectors:
//! `tests/vectors/cchain/precompile/configkey_golden.json`).
//!
//! [`PrecompileProvider`]: ava_evm_reth::PrecompileProvider

pub mod abi;
pub mod allowlist;
pub mod feemanager;
pub mod gaspricemanager;
pub mod nativeminter;
pub mod registry;
pub mod rewardmanager;
pub mod warp;
