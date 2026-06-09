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
//! # Async reactor (M7.15)
//!
//! The async-notification layer of the streaming executor (specs/11 §6, §1.5):
//!
//! * [`Eventual<T>`] — a set-once, [`tokio::sync::watch`]-backed awaitable cell
//!   (Go `eventual.Value[*Receipt]`). Keyed by tx hash in the [`Executor`]'s
//!   receipt buffer, it lets a caller await a specific tx's receipt before or
//!   after its block executes.
//! * [`HeadEvents`] — a [`tokio::sync::broadcast`] of [`ChainHeadEvent`] (Go
//!   `event.FeedOf[T]`): one chain-head event per executed block, exposed via
//!   [`Executor::subscribe_chain_head`].
//! * [`ExecutionWaiters`] — the `WaitUntil{Executed,Settled}` height watches
//!   (Go `chan struct{}` close fan-out). **Invariant 6 (specs/11 §10,
//!   atomics-before-broadcast):** the executed/settled height is advanced
//!   *before* the waiter wakes, so a poll-after-wake always observes `>=` what
//!   the broadcast announced.
//! * Graceful shutdown — a [`tokio_util::sync::CancellationToken`] plus a
//!   [`tokio_util::task::TaskTracker`] on the [`Executor`]:
//!   [`Executor::shutdown`] cancels, lets in-flight tasks finish, and
//!   `tracker.wait()`s for the drain.
//!
//! # Deferred to M7.26
//!
//! The bounded-`mpsc` queue + the spawned `processQueue` task *loop* (the
//! backpressure path that `await`s on `enqueue` when full) is M7.26. M7.15
//! provides the notification/shutdown primitives + chain-head emission wired
//! into the synchronous [`Executor::execute_one`]; the loop that feeds it from a
//! bounded queue is built on top of these in M7.26. See the `// M7.26` markers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::cast_possible_truncation)]
#![deny(clippy::cast_sign_loss)]
#![deny(clippy::cast_possible_wrap)]

mod driver;
mod error;
mod events;
mod eventual;
mod execute_step;
mod executor;

pub use crate::driver::{
    AvaEvmDriver, BlockOutcome, EvmDriver, ExecHooks, NoopExecHooks, TxReceipt, rebuild_gas_clock,
};
pub use crate::error::{Error, Result};
pub use crate::events::{ChainHeadEvent, ExecutionWaiters, HeadEvents};
pub use crate::eventual::Eventual;
pub use crate::execute_step::{StepOutput, execute_step};
pub use crate::executor::{Executor, ReceiptSink};
