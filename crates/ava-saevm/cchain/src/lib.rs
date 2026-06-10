// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-cchain` — the minimal EVM C-Chain on the SAE VM: hooks, atomic
//! import/export, the initialize harness, and the `/avax` API (specs/11 §8).
//!
//! M7.21 implements the SAE [`hook::PointsG`] surface as [`CChainHooks`]:
//! deterministic header building, the ACP-176 gas config after a block,
//! end-of-block mint/burn ops for atomic Import/Export of AVAX, and block
//! rebuild for verification. M7.22 added the atomic Import/Export tx codec +
//! [`State`] + [`AtomicTxpool`]. M7.23 ([`vm`]) supplies the VM `Initialize`
//! harness — composing [`ava_saevm_core::Vm`] (the `sae::Vm` analog) with the
//! C-Chain hooks + atomic txpool (specs/11 §5) — and the [`api`] `/avax`
//! JSON-RPC service.
//!
//! [`hook::PointsG`]: ava_saevm_hook::PointsG

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

pub mod api;
pub mod dynamic;
pub mod hooks;
pub mod state;
pub mod tx;
pub mod txpool;
pub mod vm;

pub use api::{AVAX_EXTENSION_PATH, AVAX_SERVICE_NAME, AvaxService};
pub use hooks::{
    AtomicOp, AtomicOpSource, BLACKHOLE_ADDR, CChainHooks, Error, GAS_CONFIG_AFTER_TARGET,
    Rebuilder,
};
pub use state::State;
pub use tx::{Credential, Export, Import, Input, Output, Tx, Unsigned};
pub use txpool::{AtomicTxpool, EvmPoolStub, WaitPool, WaitSource};
pub use vm::{CChainCoreVm, Vm};
