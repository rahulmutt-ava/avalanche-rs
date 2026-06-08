// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Stateful precompiles + registry: warp, allowlist, feemanager, nativeminter,
//! rewardmanager (G4, spec 10 §8, spec 20 §7).
//!
//! This module (M6.21) delivers the **registry + provider + height-gating +
//! fall-through** plumbing: the [`registry::PrecompileRegistry`] (address →
//! stateful precompile + activation timestamp), the
//! [`registry::AvaPrecompiles`] revm [`PrecompileProvider`] that overlays the
//! activated Avalanche precompiles on revm's standard Ethereum set, and the
//! [`registry::AvaCtxExt`] revm context extension (G10) whose `predicates` field
//! M6.22's pre-execution predicate pass fills with verified warp results. The
//! actual warp/allowlist/feemanager/nativeminter/rewardmanager precompile bodies
//! are M6.22.
//!
//! [`PrecompileProvider`]: ava_evm_reth::PrecompileProvider

pub mod registry;
