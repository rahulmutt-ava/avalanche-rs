// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-hook` — SAE lifecycle hooks: the `Points`/`BlockBuilder` trait
//! seam, the mint/burn [`Op`], and the [`Settled`] gas clock (specs/11 §9.1).
//!
//! Faithful port of `vms/saevm/hook/hook.go`. This crate provides the
//! object-safe trait seam plus the [`Op`] and [`Settled`] data types; the full
//! method bodies are implemented by the C-Chain hooks in M7.21.
//!
//! # Deferred libevm-rules plumbing
//!
//! Several Go hook methods take libevm-specific types (`params.Rules`,
//! `state.StateDB`, `*types.Header`, `*types.Block`, `block.Context`,
//! `saetypes.BlockSource`) that are not yet wired at this layer. Where a natural
//! Rust equivalent exists we use the reth aliases re-exported from
//! [`ava_saevm_types`]; for `state.StateDB` we use [`op::StateMut`]; for the
//! libevm rules / `CanExecuteTransaction` allowlist hook and `BlockSource` we
//! use placeholder associated types. See the `// TODO(M7.14/M7.21)` markers.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

pub mod op;
pub mod settled;

use ava_saevm_types::{Address, SealedHeader, U256};
use ava_vm::components::gas::Gas;

pub use crate::op::{AccountDebit, Op, OpError, StateMut};
pub use crate::settled::Settled;

/// `MinimumGasConsumption` MUST be used as the implementation for the respective
/// method on the libevm `RulesHooks`. The concrete type implementing the hooks
/// MUST propagate incoming and return arguments unchanged.
///
/// Port of Go's `hook.MinimumGasConsumption`: `ceil_div(tx_limit, LAMBDA)`.
#[must_use]
pub fn minimum_gas_consumption(tx_limit: u64) -> u64 {
    ava_saevm_intmath::ceil_div(tx_limit, ava_saevm_params::LAMBDA)
}

/// A user-defined transaction type that can be represented as an [`Op`].
///
/// Port of Go's `hook.Transaction`.
pub trait Transaction {
    /// Returns the [`Op`] representation of this transaction.
    fn as_op(&self) -> Op;
}

/// User-injected hook points which do not depend on generic types.
///
/// Port of Go's `hook.Points`. Object-safe so it can be used behind
/// `Arc<dyn Points>`.
///
/// The sync methods (`before_executing_block` / `after_executing_block` /
/// `end_of_block_ops`) are sync in Go, so they are sync here.
pub trait Points {
    /// Error type returned by fallible hook points.
    type Error;
    /// The sealed-block type the hooks operate on.
    ///
    /// Concretely `SealedBlock<RethBlock>` once wired (M7.21); kept as an
    /// associated type here so the seam need not name the reth block type.
    type Block;
    /// Receipts type produced by block execution.
    ///
    /// Placeholder until the receipt type is wired in M7.21.
    type Receipts;
    /// State handle used by execution hooks.
    ///
    /// Placeholder for libevm `params.Rules`; wired in M7.14/M7.21.
    // TODO(M7.14/M7.21): replace with the concrete reth/libevm rules type.
    type Rules;
    /// The height-indexed execution-results DB type.
    type ExecutionResultsDb;

    /// Opens and returns a height-indexed database, closed by the VM when no
    /// longer needed. It MAY use `data_dir` for persistence and MUST NOT write
    /// data outside of it.
    ///
    /// Mirrors Go's `Points.ExecutionResultsDB`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the database cannot be opened.
    fn execution_results_db(&self, data_dir: &str)
    -> Result<Self::ExecutionResultsDb, Self::Error>;

    /// Returns the gas target and configuration that should go into effect
    /// immediately after the provided header.
    ///
    /// Mirrors Go's `Points.GasConfigAfter`.
    fn gas_config_after(&self, header: &SealedHeader) -> (Gas, ava_saevm_gastime::GasPriceConfig);

    /// Returns the exact block time (Unix seconds, sub-second nanos) for the
    /// given header, as recorded in `BlockBuilder::build_header`.
    ///
    /// Mirrors Go's `Points.BlockTime` (returns a `time.Time`; here a
    /// `(unix_seconds, nanos)` pair to avoid floating point).
    fn block_time(&self, header: &SealedHeader) -> (u64, u32);

    /// Returns the [`Settled`] extra information for the settled block of the
    /// provided header. It MUST match the value passed to
    /// `BlockBuilder::build_block`.
    ///
    /// Mirrors Go's `Points.SettledBy`.
    fn settled_by(&self, header: &SealedHeader) -> Settled;

    /// Returns operations outside of the normal EVM state changes to perform
    /// while executing `block`, after regular EVM transactions. Performed during
    /// both worst-case and actual execution.
    ///
    /// Mirrors Go's `Points.EndOfBlockOps`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the end-of-block ops cannot be computed.
    fn end_of_block_ops(&self, block: &Self::Block) -> Result<Vec<Op>, Self::Error>;

    /// Mirrors the libevm `RulesAllowlistHooks.CanExecuteTransaction` so
    /// consumers can use a single concrete type for both SAE and libevm hooks.
    ///
    /// Mirrors Go's `Points.CanExecuteTransaction`. `to` is `None` for contract
    /// creation. `state` is a read-only state handle.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the transaction is not permitted.
    fn can_execute_transaction(
        &self,
        from: Address,
        to: Option<Address>,
        state: &dyn StateRead,
    ) -> Result<(), Self::Error>;

    /// Called immediately prior to executing `block`.
    ///
    /// Mirrors Go's `Points.BeforeExecutingBlock`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the pre-execution hook fails.
    fn before_executing_block(
        &self,
        rules: &Self::Rules,
        state: &mut dyn StateMut,
        block: &Self::Block,
    ) -> Result<(), Self::Error>;

    /// Called immediately after executing `block`.
    ///
    /// Mirrors Go's `Points.AfterExecutingBlock`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the post-execution hook fails.
    fn after_executing_block(
        &self,
        state: &mut dyn StateMut,
        block: &Self::Block,
        receipts: Self::Receipts,
    ) -> Result<(), Self::Error>;
}

/// A read-only state handle for [`Points::can_execute_transaction`].
///
/// Mirrors libevm's `StateReader`. The concrete revm-backed impl lands in
/// M7.14.
pub trait StateRead {
    /// Returns the balance of `a`.
    fn balance(&self, a: Address) -> U256;
    /// Returns the nonce of `a`.
    fn nonce(&self, a: Address) -> u64;
}

/// Constructs a block given its components.
///
/// Port of Go's `hook.BlockBuilder[T]`.
pub trait BlockBuilder<T: Transaction> {
    /// Error type returned by fallible builder methods.
    type Error;
    /// The sealed-block type produced by `build_block`.
    ///
    /// Concretely `SealedBlock<RethBlock>` once wired (M7.21).
    type Block;
    /// Block-context type passed to `build_block`.
    ///
    /// Placeholder for the Snowman `block.Context`; wired in M7.21.
    // TODO(M7.21): replace with the concrete consensus block-context type.
    type BlockContext;
    /// Concrete transaction type used by `build_block`.
    ///
    /// Placeholder for libevm `*types.Transaction`; wired in M7.21.
    type EvmTransaction;
    /// Concrete receipt type used by `build_block`.
    type Receipt;
    /// Source of worst-case-queue blocks used to filter end-of-block ops.
    ///
    /// Placeholder for `saetypes.BlockSource` (not yet defined in
    /// `ava_saevm_types`); wired in M7.21.
    // TODO(M7.21): replace with the concrete `BlockSource` type.
    type BlockSource;

    /// Constructs a header from `parent`.
    ///
    /// The returned header MUST have parent-hash, number, and time set
    /// appropriately. Root, gas-limit, base-fee, and gas-used will be ignored
    /// and overwritten.
    ///
    /// Mirrors Go's `BlockBuilder.BuildHeader`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the header cannot be built.
    fn build_header(&self, parent: &SealedHeader) -> Result<SealedHeader, Self::Error>;

    /// Returns the custom transactions that would be valid to include into a
    /// block being built.
    ///
    /// `header` is the block being built, `last_settled_block` the hash of the
    /// last block to settle, and `source` a block source for filtering against
    /// the worst-case queue. SAE filters any transaction whose [`Op`] cannot be
    /// safely applied to the state.
    ///
    /// Mirrors Go's `BlockBuilder.PotentialEndOfBlockOps` (an `iter.Seq[T]`;
    /// here a `Vec<T>` — lazy iteration can be revisited in M7.21).
    fn potential_end_of_block_ops(
        &self,
        header: &SealedHeader,
        last_settled_block: ava_saevm_types::B256,
        source: &Self::BlockSource,
    ) -> Vec<T>;

    /// Constructs a block with the given components. The header MAY be modified;
    /// all other arguments are read-only.
    ///
    /// Mirrors Go's `BlockBuilder.BuildBlock`.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the block cannot be built.
    #[allow(clippy::too_many_arguments)]
    fn build_block(
        &self,
        header: SealedHeader,
        block_ctx: &Self::BlockContext,
        txs: &[Self::EvmTransaction],
        receipts: &[Self::Receipt],
        end_of_block_ops: &[T],
        settled: Settled,
    ) -> Result<Self::Block, Self::Error>;
}

/// User-injected hook points combining [`Points`] and [`BlockBuilder`].
///
/// Port of Go's `hook.PointsG[T]`. Directly using this as a [`BlockBuilder`] is
/// indicative of locally building a block; `block_rebuilder_from` reconstructs a
/// block built elsewhere during verification.
pub trait PointsG<T: Transaction>: Points + BlockBuilder<T> {
    /// The [`BlockBuilder`] type returned by `block_rebuilder_from`.
    type Rebuilder: BlockBuilder<T>;

    /// Returns a [`BlockBuilder`] that will attempt to reconstruct `block`. If
    /// the block is valid for inclusion, the returned builder MUST be able to
    /// reconstruct an identical block.
    ///
    /// Mirrors Go's `PointsG.BlockRebuilderFrom`.
    ///
    /// # Errors
    ///
    /// Returns `<Self as BlockBuilder<T>>::Error` if a rebuilder cannot be
    /// constructed.
    fn block_rebuilder_from(
        &self,
        block: &<Self as BlockBuilder<T>>::Block,
    ) -> Result<Self::Rebuilder, <Self as BlockBuilder<T>>::Error>;
}
