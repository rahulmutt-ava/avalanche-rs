// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain transaction executors (`vms/platformvm/txs/executor`, specs 08 §2.4).
//!
//! The executors are [`Visitor`](crate::txs::Visitor) implementations that
//! verify and apply a tx to a [`Diff`](crate::state::diff::Diff). They share:
//!
//! - [`Backend`] — the node-wide context (fork schedule, staking/fee config,
//!   chain ids, fx spend gate, bootstrapped flag).
//! - [`subnet_tx_verification`] — subnet/owner authorization
//!   ([`verify_subnet_authorization`], [`verify_authorization`]).
//! - [`staker_tx_verification`] — permissionless-staker semantic checks
//!   ([`verify_add_permissionless_validator`] / `_delegator`,
//!   [`verify_staker_start_time`], [`get_validator`]).
//! - [`state_changes`] — the fork-selected fee calculator and the single-asset
//!   flow check.
//!
//! M4.16 ships the standard (decision) executor here. The sibling tasks add new
//! files purely additively: M4.17 (`proposal_tx_executor`), M4.18
//! (`atomic_tx_executor`), and M4.19 (`l1_executor`) reuse this `Backend` and
//! these `pub(crate)` verification helpers without editing the standard
//! executor's files.

pub mod backend;
pub mod staker_tx_verification;
pub mod standard_tx_executor;
pub mod state_changes;
pub mod subnet_tx_verification;

pub use backend::{Backend, StakingConfig, UpgradeSchedule};
pub use standard_tx_executor::{AtomicRequests, StandardTxExecutor, StandardTxOutputs};

// The shared verification helpers are `pub(crate)` in their submodules; the
// sibling executors (M4.17/M4.18/M4.19) reach them via the (public) submodule
// paths, e.g. `crate::txs::executor::staker_tx_verification::verify_add_*` and
// `subnet_tx_verification::verify_subnet_authorization`. They are intentionally
// not re-exported here to keep `mod.rs` free of sibling-specific surface.
