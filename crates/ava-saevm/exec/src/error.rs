// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The executor error model (specs/11 §11): a [`Fatal`](Error::Fatal) variant
//! for consensus-critical failures (a transaction that *errored* rather than
//! reverted, a parent-hash mismatch, a broken durability ordering) that stops
//! the single execution thread, distinguished from recoverable
//! lower-layer failures.
//!
//! Port of `vms/saevm/saexec/execution.go::errFatal`. The Go reference logs a
//! `Fatal` execution error with the emergency-playbook link and terminates the
//! executor loop; here the variant is surfaced to the caller, which makes the
//! same stop-the-executor decision.

use ava_saevm_types::B256;

/// The crate result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure during the SAE execute step (specs/11 §6.1/§11).
///
/// [`Error::Fatal`] is consensus-critical and MUST stop the execution thread
/// (Go `errFatal`): the executor cannot make forward progress without operator
/// intervention. All other variants are recoverable lower-layer errors that
/// nonetheless abort the current block.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A consensus-critical failure that stops the executor (Go `errFatal`).
    ///
    /// Raised when: a transaction *errored* (vs. reverted) during EVM
    /// execution, an end-of-block op could not be applied, the parent-hash
    /// sanity check failed, or a hook returned an error. The wrapped message
    /// preserves the originating context for the emergency playbook.
    #[error("fatal execution error: {0}")]
    Fatal(String),

    /// The realised base fee (or an op burner balance) violated the
    /// builder-agreed worst-case bound (specs/11 §6.1). Test-fatal; surfaced
    /// distinctly so callers can assert on it.
    #[error("worst-case bound violated: {0}")]
    WorstCase(#[from] ava_saevm_worstcase::Error),

    /// A Firewood / state-DB layer failure (open, propose, or commit) from the
    /// saedb [`Tracker`](ava_saevm_db::Tracker).
    #[error("state db: {0}")]
    StateDb(#[from] ava_saevm_db::Error),

    /// A [`Block`](ava_saevm_blocks::Block) lifecycle transition failed (e.g. a
    /// re-execution, or the `mark_executed` persist step).
    #[error("block lifecycle: {0}")]
    Lifecycle(#[from] ava_saevm_blocks::Error),

    /// [`Executor::execute_one`](crate::Executor::execute_one) was called before
    /// the executor was seeded with a `last_executed` block. The VM seeds the
    /// executor at init (the genesis/synchronous block, M7.18); reaching this is
    /// a programming error. Treated as fatal.
    #[error("executor not seeded: execute_one called before a last-executed block was installed")]
    NotSeeded,

    /// The parent's hash does not match this block's `parent_hash`. Distinct
    /// from a generic [`Error::Fatal`] so the executor's parent-hash sanity
    /// check is testable; it is treated as fatal by the executor loop.
    #[error("executing block built on parent {parent:#x} when last executed {last:#x}")]
    ParentMismatch {
        /// The `parent_hash` declared by the block being executed.
        parent: B256,
        /// The hash of the executor's current `last_executed` block.
        last: B256,
    },
}

impl Error {
    /// Reports whether this error must stop the executor (Go `errFatal`
    /// semantics). A reverted transaction is normal and never produces an
    /// [`Error`]; an *errored* transaction, a parent mismatch, a hook failure,
    /// or a broken durability ordering all stop the executor.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Error::Fatal(_) | Error::ParentMismatch { .. } | Error::NotSeeded
        )
    }
}
