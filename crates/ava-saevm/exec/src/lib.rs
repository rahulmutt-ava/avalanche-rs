// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-exec` — the single-task streaming executor over the `ava-evm`
//! revm + Firewood reuse APIs (specs/11 §6 / §6.1).
//!
//! # The execute step (specs/11 §6.1)
//!
//! [`execute_step`] is a **pure** function of `(ordered block, parent state,
//! chain config, hooks)` — no wall-clock enters any consensus output and no
//! unsorted map is iterated (specs/00 §6.1 determinism). It ports the 10-step
//! Go `saexec/execution.go::Execute` + `afterExecution` sequence:
//!
//! 1. Parent-hash sanity (`last_executed.hash() == block.parent_hash()`; else
//!    [`Error::ParentMismatch`], fatal).
//! 2. Clone the parent gas clock; `before_block(block_time)`.
//! 3. Open the parent post-execution state via the saedb
//!    [`Tracker`](ava_saevm_db::Tracker).
//! 4. `before_executing_block` hook.
//! 5. `base_fee = gas_clock.price()`; check against the worst-case bound
//!    ([`ava_saevm_worstcase::check_base_fee_bound`]).
//! 6. Per tx: run revm via the `ava-evm` reuse seam, tick the per-tx clock,
//!    advance the block's interim execution time, publish receipts.
//! 7. `end_of_block_ops` → apply mint/burn [`Op`](ava_saevm_hook::Op)s.
//! 8. `after_executing_block` hook.
//! 9. `gas_clock.after_block(...)`.
//! 10. Commit in strict **D→M→I→X** / CC-ORDER (specs/27 §2.4): propose the
//!     bundle → [`Tracker::maybe_commit`](ava_saevm_db::Tracker::maybe_commit) →
//!     [`Tracker::track`](ava_saevm_db::Tracker::track) →
//!     [`Block::mark_executed`](ava_saevm_blocks::Block::mark_executed) (which
//!     itself runs D→M→I→X) → emit events.
//!
//! # The "one EVM, two drivers" reuse contract (specs/00 §11.1.5)
//!
//! SAE is the **async** driver; it reuses `ava-evm`'s revm+Firewood execution
//! path rather than re-implementing the EVM. The seam is abstracted behind the
//! [`EvmDriver`] trait so the execute step is testable without spinning up a
//! live revm: the production impl ([`AvaEvmDriver`]) wraps
//! [`ava_evm::AvaEvmConfig`] + [`ava_evm::FirewoodStateProvider`], driving
//! [`AvaEvmConfig`](ava_evm::AvaEvmConfig)'s
//! [`execute_batch`](ava_evm_reth::ExternalConsensusExecutor::execute_batch) and
//! [`ava_evm::FirewoodStateProvider::propose_from_bundle`].
//!
//! # Deferred to M7.15
//!
//! The full async reactor — the bounded `mpsc` queue + `processQueue` task, the
//! `Eventual<Receipt>` receipt buffer, `ChainHead` / `WaitUntil*` event
//! plumbing, `CancellationToken` / `JoinHandle` / `TaskTracker` — is M7.15.
//! This task delivers the synchronous execute step plus the [`Executor`]
//! skeleton and a minimal receipt sink + chain-head notify the reactor will
//! hang off of. See the `// TODO(M7.15)` markers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::cast_possible_truncation)]
#![deny(clippy::cast_sign_loss)]
#![deny(clippy::cast_possible_wrap)]

mod driver;
mod error;
mod execute_step;
mod executor;

pub use crate::driver::{
    AvaEvmDriver, BlockOutcome, EvmDriver, ExecHooks, NoopExecHooks, TxReceipt, rebuild_gas_clock,
};
pub use crate::error::{Error, Result};
pub use crate::execute_step::{StepOutput, execute_step};
pub use crate::executor::{Executor, ReceiptSink};
