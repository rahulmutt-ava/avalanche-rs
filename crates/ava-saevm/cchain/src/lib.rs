// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-cchain` — the minimal EVM C-Chain on the SAE VM: hooks, atomic
//! import/export, the initialize harness, and the `/avax` API (specs/11 §8).
//!
//! M7.21 implements the SAE [`hook::PointsG`] surface as [`CChainHooks`]:
//! deterministic header building, the ACP-176 gas config after a block,
//! end-of-block mint/burn ops for atomic Import/Export of AVAX, and block
//! rebuild for verification. The atomic Import/Export tx codec + txpool (the
//! real source of [`AtomicOp`]s) and the VM `Initialize` harness + `/avax` API
//! land in later M7 tasks (M7.22/M7.23); see `plan/M7-saevm.md`.
//!
//! [`hook::PointsG`]: ava_saevm_hook::PointsG

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod hooks;

pub use hooks::{
    AtomicOp, AtomicOpSource, BLACKHOLE_ADDR, CChainHooks, Error, GAS_CONFIG_AFTER_TARGET,
    Rebuilder,
};
