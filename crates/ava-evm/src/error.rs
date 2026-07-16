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

use ava_evm_reth::{Address, AvaEvmError, B256, BlockExecutionError, ProviderError, U256};

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

    /// A `SendWarpMessage` log failed to decode/record on block accept
    /// (`handlePrecompileAccept`, M6.31, spec 20 §3.1).
    #[error("warp precompile accept: {0}")]
    Warp(#[from] ava_warp::Error),

    // --- Cancun syntactic header clamp (coreth `wrapped_block.go:493-518`;
    // M9.15 task 8f). Messages mirror coreth's sentinels so mixed-net
    // operators see the same rejection text from both implementations.
    /// `errMissingParentBeaconRoot` — Cancun header lacks `parentBeaconRoot`.
    #[error("header is missing parentBeaconRoot")]
    MissingParentBeaconRoot,

    /// `errParentBeaconRootNonEmpty` — Cancun `parentBeaconRoot` must be the
    /// zero hash on Avalanche (no beacon chain). Carries the offending root.
    #[error("invalid non-empty parentBeaconRoot: have {0}, expected empty hash")]
    ParentBeaconRootNonEmpty(B256),

    /// `errInvalidParentBeaconRootBeforeCancun` — the field must be absent
    /// pre-Cancun.
    #[error("invalid parentBeaconRoot before cancun")]
    ParentBeaconRootBeforeCancun,

    /// `errBlobGasUsedNilInCancun` — Cancun header lacks `blobGasUsed`.
    #[error("blob gas used must not be nil in Cancun")]
    BlobGasUsedNilInCancun,

    /// `errBlobsNotEnabled` — Cancun `blobGasUsed` must be 0 (blob txs are not
    /// supported on Avalanche networks). Carries the declared blob gas.
    #[error("blobs not enabled on avalanche networks: used {0} blob gas, expected 0")]
    BlobsNotEnabled(u64),

    /// `errInvalidBlobGasUsedBeforeCancun` — the field must be absent pre-Cancun.
    #[error("invalid blobGasUsed before cancun")]
    BlobGasUsedBeforeCancun,

    /// `errInvalidExcessBlobGas` — Cancun `excessBlobGas` must be present and 0.
    /// Carries the offending value (`None` = missing).
    #[error("invalid excessBlobGas: have {0:?}, expected 0")]
    InvalidExcessBlobGas(Option<u64>),

    /// `errInvalidExcessBlobGasBeforeCancun` — the field must be absent
    /// pre-Cancun.
    #[error("invalid excessBlobGas before cancun")]
    ExcessBlobGasBeforeCancun,

    /// coreth `wrapped_block.go:420-421` — a C-Chain header's `MixDigest` must
    /// be the zero hash (Avalanche has no beacon randomness). Ungated: applies to
    /// every non-genesis block. Carries the offending digest. Guarding it closes
    /// an adversarial PREVRANDAO fail-open (a Byzantine block with a nonzero
    /// mix digest + a PREVRANDAO-reading tx that Go rejects and Rust would run).
    #[error("invalid mix digest: {0}")]
    InvalidMixDigest(B256),

    /// `ValidateBody` blob-count parity (coreth `core/block_validator.go:100-104`):
    /// the body's blob hashes × `DATA_GAS_PER_BLOB` must equal the header's
    /// `blobGasUsed` — with the Cancun clamp forcing 0, any type-3 blob tx
    /// mismatches and the block is rejected before execution.
    #[error("blob gas used mismatch (header {header}, calculated {calculated})")]
    BlobGasUsedMismatch {
        /// The header-declared blob gas (0 under the clamp).
        header: u64,
        /// The blob gas implied by the body's blob hashes.
        calculated: u64,
    },

    // --- Remaining coreth `wrappedBlock.syntacticVerify` port (M9.15 task L1,
    // `wrapped_block.go:398-527`). Difficulty==1 and `VerifyExtra` are
    // deliberately NOT ported yet (Task 5) — the builder still stamps
    // difficulty 0, so enforcing them now would reject Rust's own blocks.
    /// coreth `wrapped_block.go:412` — block number exceeds uint64.
    /// `AvaHeader::number` already decodes as a Rust `u64` (never a wider
    /// integer), so this can never fire in practice; kept for Go check-order
    /// parity and Task 6's rejection-class mapping.
    #[error("invalid block number: {0}")]
    InvalidBlockNumber(U256),

    /// coreth `wrapped_block.go:418` — header nonce must be 0.
    #[error("expected nonce to be 0 but got {0}: invalid nonce")]
    InvalidNonce(u64),

    /// coreth `wrapped_block.go:434` — block body extension version must be 0.
    #[error("invalid version: {0}")]
    InvalidBlockVersion(u32),

    /// coreth `wrapped_block.go:439` — header txsHash vs body mismatch.
    #[error("invalid txs hash {header} does not match calculated txs hash {calculated}")]
    TxRootMismatch {
        /// The header-declared transactions root.
        header: B256,
        /// The root recomputed from the body's transaction list.
        calculated: B256,
    },

    /// coreth `wrapped_block.go:444`/`:453` — header uncleHash vs the
    /// (structurally empty, block.rs decode-enforced) body uncle list.
    #[error("invalid uncle hash {0} does not match calculated uncle hash")]
    InvalidUncleHash(B256),

    /// coreth `wrapped_block.go:449` — coinbase must be the blackhole address.
    #[error("invalid coinbase {0} does not match required blackhole address")]
    InvalidCoinbase(Address),

    /// coreth `wrapped_block.go:458-473` — tx gas price below the phase
    /// minimum (pre-AP1: `ap0.MinGasPrice`; pre-AP3: `ap1.MinGasPrice`).
    #[error("block contains tx {tx} with gas price too low ({have} < {min})")]
    GasPriceTooLow {
        /// The offending transaction's hash.
        tx: B256,
        /// Its declared gas price (`tx.GasPrice()` — the fee cap for
        /// dynamic-fee txs).
        have: u128,
        /// The phase-minimum gas price it fell below.
        min: u128,
    },

    /// coreth `wrapped_block.go:486-495` — `BlockGasCost` nil/oversized at
    /// AP4+. Carries the offending value (`None` = missing).
    #[error("invalid block gas cost: {0:?}")]
    InvalidBlockGasCost(Option<U256>),
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

// The `EvmVm` `ChainVm` adapter (M6.10) returns the engine-facing `ava_vm` /
// `ava_snow` error types from the block lifecycle + the VM surface; map the
// C-Chain `Error` onto those crates' (closed, non-exhaustive) enums. The orphan
// rule permits these impls because the source type is local. Mirrors the
// ava-avm / ava-platformvm precedent (their `error.rs`).
//
// Neither `ava_vm::Error` nor `ava_snow::Error` exposes a free-form `Other`
// variant. A lookup miss is the only error with an exact engine analogue, so
// `MissingProposal` (the "block not in the processing tree" case the adapter
// surfaces from `get_block`/`accept`) round-trips to `ava_vm::Error::NotFound`;
// every other C-Chain error collapses onto the nearest carrying variant.
impl From<Error> for ava_vm::error::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::MissingProposal(_) => ava_vm::error::Error::NotFound,
            // No generic string variant exists on `ava_vm::Error`; surface a
            // stable, descriptive static message (the detailed message stays on
            // the C-Chain log path, not the engine-facing error).
            _ => ava_vm::error::Error::InvalidComponent("evm vm/block error"),
        }
    }
}

impl From<Error> for ava_snow::error::Error {
    fn from(e: Error) -> Self {
        // `ava_snow::Error::ParametersInvalid(String)` is the only string-carrying
        // variant; reuse it to preserve the C-Chain error message on the critical
        // verify/accept path (a returned `Err` halts the chain).
        ava_snow::error::Error::ParametersInvalid(format!("evm: {e}"))
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

        // Remaining `syntacticVerify` port sentinels (M9.15 task L1).
        assert_matches!(
            Error::InvalidBlockNumber(U256::ZERO),
            Error::InvalidBlockNumber(_)
        );
        assert_matches!(Error::InvalidNonce(1), Error::InvalidNonce(_));
        assert_matches!(Error::InvalidBlockVersion(1), Error::InvalidBlockVersion(_));
        assert_matches!(
            Error::TxRootMismatch {
                header: B256::ZERO,
                calculated: B256::ZERO
            },
            Error::TxRootMismatch { .. }
        );
        assert_matches!(
            Error::InvalidUncleHash(B256::ZERO),
            Error::InvalidUncleHash(_)
        );
        assert_matches!(
            Error::InvalidCoinbase(Address::ZERO),
            Error::InvalidCoinbase(_)
        );
        assert_matches!(
            Error::GasPriceTooLow {
                tx: B256::ZERO,
                have: 0,
                min: 1
            },
            Error::GasPriceTooLow { .. }
        );
        assert_matches!(
            Error::InvalidBlockGasCost(None),
            Error::InvalidBlockGasCost(_)
        );
    }
}
