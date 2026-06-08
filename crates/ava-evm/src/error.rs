// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-evm::Error` — the C-Chain error model (spec 10 §11.2).
//!
//! Per overview §7.1, this enum preserves the coreth/atomic **sentinels** as
//! variants and matches on them where Go uses `errors.Is`. reth/revm execution
//! errors and reth provider errors are wrapped via `#[from]` (through the
//! [`ava_evm_reth`] facade — this crate never names a `reth_*` error directly).
//!
//! All balance/fee arithmetic in this crate is **checked** (overflow → a typed
//! error, never silent wrap — overview §6.1); [`Error::FeeOverflow`] is the
//! sentinel that surfaces such a failure.

use ava_evm_reth::{AvaEvmError, B256, BlockExecutionError, ProviderError};

/// C-Chain VM error. Sentinel variants mirror coreth's `errors.Is` targets so
/// callers can `assert_matches!` / `matches!` on them exactly as Go does.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `ErrWrongNetworkID` — tx carries a network ID that is not this chain's.
    #[error("wrong network ID")]
    WrongNetworkId,

    /// `ErrNilTx` — a nil/empty transaction was supplied where one is required.
    #[error("nil transaction")]
    NilTx,

    /// `ErrNoValueOutput` — an atomic output carries zero value.
    #[error("atomic output has no value")]
    NoValueOutput,

    /// `ErrNoValueInput` — an atomic input carries zero value.
    #[error("atomic input has no value")]
    NoValueInput,

    /// `ErrNoGasUsed` — a tx that must consume gas reported none.
    #[error("no gas used")]
    NoGasUsed,

    /// `errNilBaseFee` — base fee absent where the active fork requires it
    /// (pre-AP3 has no base fee; AP3+ must).
    #[error("nil base fee")]
    NilBaseFee,

    /// `ErrFeeOverflow` — checked fee/balance arithmetic overflowed.
    #[error("fee overflow")]
    FeeOverflow,

    /// `ErrConflictingAtomicInputs` — two atomic txs (in a block or across its
    /// ancestry / shared memory) consume the same source UTXO.
    #[error("conflicting atomic inputs")]
    ConflictingAtomicInputs,

    /// The C-Chain genesis JSON (coreth `core.Genesis`) failed to parse — bad
    /// JSON, a malformed hex field, or a missing required field (spec 10 §11.1).
    #[error("invalid C-Chain genesis: {0}")]
    GenesisParse(String),

    /// No stashed Firewood proposal exists for the given pre-commit state root
    /// (the `accept` path expected a proposal that `verify` should have
    /// stashed). Carries the missing root.
    #[error("missing stashed proposal for state root {0}")]
    MissingProposal(B256),

    /// A reth/revm block-execution failure (invalid tx, gas, state, ...),
    /// wrapped through the facade.
    #[error(transparent)]
    Execution(#[from] BlockExecutionError),

    /// A reth provider/state-read failure, wrapped through the facade.
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// C-Chain VM result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Folds a facade [`AvaEvmError`] (the executor boundary error) into the C-Chain
/// [`Error`] model: block-execution / provider errors map to their existing
/// sentinel-carrying variants; fee overflow and fork-incompatibility map to
/// [`Error::FeeOverflow`] / a provider error so the lifecycle (`verify`) can use
/// `?` directly (spec 10 §11.2).
impl From<AvaEvmError> for Error {
    fn from(err: AvaEvmError) -> Self {
        match err {
            AvaEvmError::BlockExecution(e) => Error::Execution(e),
            AvaEvmError::Provider(e) => Error::Provider(e),
            AvaEvmError::FeeOverflow => Error::FeeOverflow,
            AvaEvmError::IncompatibleFork { fork } => {
                Error::Provider(ProviderError::Database(ava_evm_reth::DatabaseError::Other(
                    format!("incompatible fork: {fork} already activated"),
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;

    /// The coreth/atomic sentinels are constructible and match by pattern
    /// (the `errors.Is` parity contract, spec 10 §11.2), and a facade
    /// `BlockExecutionError` folds in via `#[from]`.
    #[test]
    fn sentinels_match_via_matches() {
        assert_matches!(Error::WrongNetworkId, Error::WrongNetworkId);
        assert_matches!(Error::NilTx, Error::NilTx);
        assert_matches!(Error::NilBaseFee, Error::NilBaseFee);
        assert_matches!(Error::FeeOverflow, Error::FeeOverflow);
        assert_matches!(
            Error::ConflictingAtomicInputs,
            Error::ConflictingAtomicInputs
        );
        assert_matches!(
            Error::MissingProposal(B256::ZERO),
            Error::MissingProposal(_)
        );

        // `#[from]` wrap of a facade BlockExecutionError.
        let e: Error = BlockExecutionError::msg("boom").into();
        assert_matches!(e, Error::Execution(_));
    }
}
