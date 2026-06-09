// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-db` — the SAE storage tracker: consensus-versus-execution state
//! over Firewood revisions (CC-ORDER, no reorgs; specs/11 §7, specs/27 §2.4).
//!
//! SAE keeps two logically distinct kinds of state (specs/11 §7):
//!
//! - **Consensus state** (ordering): canonical hashes, head/finalized pointers,
//!   block bodies — the `rawdb`-style KV over `ava-database`. Written on
//!   `AcceptBlock` (out of scope here).
//! - **Execution state** (the EVM trie): account/storage state keyed by
//!   **state root**, in Firewood via [`ava_evm::FirewoodStateProvider`]. Written
//!   on execution-commit, *behind* the consensus frontier.
//! - **Height-indexed [`ExecutionResults`]**: the per-block executed-artefact
//!   blob (`{gas_time, base_fee, receipt_root, post_state_root}`) over a
//!   `HeightIndex`, so executed results survive restart independently of the
//!   trie commit cadence. Defined in `ava-saevm-types` (M7.8) and re-exported
//!   here so the `saedb` surface is the full specs/11 §7 storage model.
//!
//! [`Tracker`] owns the Firewood side: it implements the **commit policy**
//! (archival vs commit-interval), a **ref-count layer** over retained revisions
//! bounded by the consensus-critical window (`LastExecuted..LastSettled`), and
//! the no-reorg **flatten-on-close** behaviour. See [`Tracker`] for the full
//! contract, including the CC-ORDER (specs/27 §2.4) invariant.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::cast_possible_truncation)]
#![deny(clippy::cast_sign_loss)]
#![deny(clippy::cast_possible_wrap)]

mod tracker;

pub use tracker::{Config, DEFAULT_COMMIT_INTERVAL, Error, Result, StateDb, Tracker};

// The height-indexed per-block executed-artefact store (specs/11 §7, the third
// storage kind) is defined in `ava-saevm-types` (M7.8); re-exported so the
// `saedb` surface presents the full §7 storage model in one place.
pub use ava_saevm_types::{ExecutionResults, ExecutionResultsDb};
