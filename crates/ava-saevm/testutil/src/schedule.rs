// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`PipelineSchedule`] injection seam (specs/11 §6.1, §10 invariant 11).
//!
//! SAE's determinism invariant (specs/11 §10 inv 11) says the executor must
//! settle to **identical state regardless of pipeline scheduling**. The only
//! scheduling axis the synchronous execute step exposes is *when the Firewood
//! commit happens* — the saedb [`Config`] commit policy
//! (archival, commit-every-block, versus deferred to a wide interval boundary).
//! Everything else that feeds a consensus output (the gas clock, the worst-case
//! base-fee bound, the per-tx interim-execution-time tick) is a pure function of
//! the block's inputs and does not vary with scheduling.
//!
//! [`PipelineSchedule`] is the test-only enum that names a concrete schedule and
//! maps it to a saedb [`Config`]. A determinism property test runs the *same*
//! block program under two different schedules and asserts the settled outputs
//! (post-state root, derived receipt root, gas-time, base fee, executed height)
//! are byte-identical.
//!
//! Note on interim-execution-time: the pure step
//! (`ava_saevm_exec::execute_step`) always ticks
//! `Block::set_interim_execution_time`
//! per-transaction — it is observational (specs/11 §6.1 step 6) and never feeds a
//! consensus output, so there is no "mid-block vs end" axis to inject at the exec
//! layer. The only schedule axis that changes *when work happens* without
//! changing the inputs is therefore the commit cadence, encoded below.

use ava_saevm_db::Config;

/// A forced pipeline schedule for the SAE executor determinism gate.
///
/// Each variant changes only **when** the durable Firewood commit happens, never
/// the execution inputs. The determinism invariant (specs/11 §10 inv 11) requires
/// the settled state to be identical across every variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineSchedule {
    /// Commit the execution root on **every** block (saedb archival mode). The
    /// tightest commit cadence: every executed root is immediately durable.
    CommitEveryBlock,
    /// Defer the commit to a wide interval boundary (saedb commit-interval mode):
    /// the settled root is committed only when `height % interval == 0`. The
    /// `interval` is the (non-zero) commit cadence; a wide interval means most
    /// blocks are pipelined no-op commits.
    DeferCommit {
        /// The (non-zero) commit interval passed to
        /// [`Config::interval`](ava_saevm_db::Config::interval).
        interval: u64,
    },
}

impl PipelineSchedule {
    /// The two canonical schedules the determinism gate contrasts: the tightest
    /// commit cadence (every block) versus a deferred wide-interval commit.
    #[must_use]
    pub fn contrasting_pair() -> [Self; 2] {
        [Self::CommitEveryBlock, Self::DeferCommit { interval: 4096 }]
    }

    /// Maps this schedule to the saedb [`Config`] the executor's
    /// [`Tracker`](ava_saevm_db::Tracker) is built with.
    #[must_use]
    pub fn db_config(self) -> Config {
        match self {
            Self::CommitEveryBlock => Config::archival(),
            Self::DeferCommit { interval } => Config::interval(interval),
        }
    }
}
