// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-avm` error model (specs/09 §11).
//!
//! Per-crate [`Error`] (`thiserror`) with one variant per Go `vms/avm` sentinel
//! (`errAssetIDMismatch`, `errNotAnAsset`, `errIncompatibleFx`, `errUnknownFx`,
//! `errDoubleSpend`, `errNoImportInputs`, `errNoExportOutputs`, the
//! name/symbol/denomination family, …), so tests can `assert_matches!` exactly
//! where Go uses `errors.Is` (the `ErrorIs` lint posture in 00/02).
//!
//! Codec errors and the shared fx/`verify` errors (which live on
//! [`ava_vm::error::Error`] and are re-exported by `ava-secp256k1fx`) are folded
//! in via `#[from]` so a single `Result` flows through parse → verify → execute.
//! Variants grow as later wave tasks (syntactic/semantic verify, executor,
//! blocks) need new sentinels — mirroring the per-crate growth in `ava-platformvm`.

use thiserror::Error;

/// The `ava-avm` result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// X-Chain (AVM) errors — one variant per preserved Go sentinel (specs/09 §11).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    // ---- value / fx routing (semantic verify) ----------------------------
    /// `errAssetIDMismatch` — a consumed UTXO's asset id differs from the input.
    #[error("asset ID of input does not match UTXO")]
    AssetIdMismatch,
    /// `errNotAnAsset` — the referenced tx is not a `CreateAssetTx`.
    #[error("referenced tx is not an asset")]
    NotAnAsset,
    /// `errIncompatibleFx` — no `InitialState` for the routed fx index.
    #[error("incompatible fx")]
    IncompatibleFx,
    /// `errUnknownFx` — the output/input/credential type has no registered fx.
    #[error("unknown fx")]
    UnknownFx,
    /// `errWrongNumberOfCredentials` — creds count != inputs+ops count.
    #[error("wrong number of credentials")]
    WrongNumberOfCredentials,
    /// `errInsufficientFunds` — consumed value < produced value + fee.
    #[error("insufficient funds")]
    InsufficientFunds,
    /// `errSpendOverflow` — value summation overflowed `u64`.
    #[error("spend overflowed")]
    SpendOverflow,
    /// `secp256k1fx.ErrMismatchedAmounts` — a consumed UTXO's amount differs
    /// from the spending input's amount (the avm-side fx spend check, 09 §4.1).
    #[error("utxo amount and input amount are not equal")]
    MismatchedAmounts,
    /// `secp256k1fx.ErrWrongMintCreated` — a mint operation produced a mint
    /// output whose owners differ from the consumed `MintOutput` UTXO's.
    #[error("wrong mint output created from the operation")]
    WrongMintCreated,
    /// `propertyfx.errWrongMintOutput` — a propertyfx mint operation's produced
    /// mint output owners differ from the consumed `MintOutput` UTXO's
    /// (specs 09 §4.3, FX-AVM-1).
    #[error("wrong mint output provided")]
    WrongMintOutput,
    /// `nftfx`/`propertyfx` `errWrongUTXOType` — the consumed UTXO is not the
    /// output type the operation requires (specs 09 §4.2/§4.3).
    #[error("wrong utxo type")]
    WrongUtxoType,
    /// `nftfx.errCantTransfer` / `propertyfx.errCantTransfer` — `VerifyTransfer`
    /// is unsupported by the nft and property fxs (specs 09 §4.2/§4.3, FX-AVM-1).
    #[error("cant transfer with this fx")]
    CantTransfer,
    /// `verify.ErrSameChainID` — an import/export references this very chain
    /// (the `SameSubnet` gate; specs 09 §6.2).
    #[error("same chainID")]
    SameChainId,
    /// `verify.ErrMismatchedSubnetIDs` — the import/export peer chain is in a
    /// different subnet (the `SameSubnet` gate; specs 09 §6.2).
    #[error("mismatched subnetIDs")]
    MismatchedSubnetIds,

    // ---- input / output / operation structure (syntactic verify) ---------
    /// `errDoubleSpend` — an input id appears more than once.
    #[error("double spend")]
    DoubleSpend,
    /// `errNoImportInputs` — an `ImportTx` has no imported inputs.
    #[error("no import inputs")]
    NoImportInputs,
    /// `errNoExportOutputs` — an `ExportTx` has no exported outputs.
    #[error("no export outputs")]
    NoExportOutputs,
    /// `errOutputsNotSorted` — `outs` are not in canonical sorted order.
    #[error("outputs not sorted")]
    OutputsNotSorted,
    /// `avax.ErrInputsNotSortedUnique` — `ins` are not sorted and unique.
    #[error("inputs not sorted and unique")]
    InputsNotSortedUnique,
    /// `avax.ErrMemoTooLarge` — the memo field exceeds `MaxMemoSize` (256).
    #[error("memo exceeds maximum length")]
    MemoTooLarge,
    /// `errNotSortedAndUniqueUTXOIDs` — operation utxo ids not sorted-unique.
    #[error("utxo IDs not sorted and unique")]
    NotSortedAndUniqueUtxoIds,
    /// `errInitialStatesNotSortedUnique`.
    #[error("initial states not sorted and unique")]
    InitialStatesNotSortedUnique,
    /// `errOperationsNotSortedUnique`.
    #[error("operations not sorted and unique")]
    OperationsNotSortedUnique,
    /// `errNilInitialState`.
    #[error("nil initial state is not valid")]
    NilInitialState,
    /// `errNilOperation`.
    #[error("nil operation is not valid")]
    NilOperation,
    /// `errNilFxOutput`.
    #[error("nil fx output is not valid")]
    NilFxOutput,
    /// `errNilFxOperation`.
    #[error("nil fx operation is not valid")]
    NilFxOperation,
    /// `errNoOperations` — an `OperationTx` has no operations.
    #[error("an operationTx must have at least one operation")]
    NoOperations,
    /// `errNoFxs` — the VM was configured with no fxs.
    #[error("no feature extensions specified")]
    NoFxs,

    // ---- nftfx payload ---------------------------------------------------
    /// `errPayloadTooLarge` — nftfx payload exceeds the 1 KiB limit.
    #[error("nftfx payload exceeds maximum size")]
    PayloadTooLarge,

    // ---- nftfx operation verification (M5.7, vms/nftfx/fx.go) ------------
    /// `nftfx.errWrongUniqueID` — the operation's `group_id` differs from the
    /// consumed nft UTXO's `group_id`.
    #[error("wrong unique ID provided")]
    WrongUniqueId,
    /// `nftfx.errWrongBytes` — a transfer operation's output `payload` differs
    /// from the consumed `TransferOutput` UTXO's `payload`.
    #[error("wrong bytes provided")]
    WrongBytes,

    // ---- CreateAssetTx name / symbol / denomination ----------------------
    /// `errNameTooLong`.
    #[error("asset name is too long")]
    NameTooLong,
    /// `errNameTooShort`.
    #[error("asset name is too short")]
    NameTooShort,
    /// `errSymbolTooLong`.
    #[error("asset symbol is too long")]
    SymbolTooLong,
    /// `errSymbolTooShort`.
    #[error("asset symbol is too short")]
    SymbolTooShort,
    /// `errDenominationTooLarge`.
    #[error("denomination is too large")]
    DenominationTooLarge,
    /// `errIllegalNameCharacter`.
    #[error("asset name contains an illegal character")]
    IllegalNameCharacter,
    /// `errIllegalSymbolCharacter`.
    #[error("asset symbol contains an illegal character")]
    IllegalSymbolCharacter,
    /// `errUnexpectedWhitespace` — leading/trailing whitespace in name.
    #[error("unexpected whitespace provided")]
    UnexpectedWhitespace,
    /// `errAddressesCantMintAsset`.
    #[error("addresses cannot mint asset")]
    AddressesCantMintAsset,
    /// `errGenesisAssetMustHaveState`.
    #[error("genesis asset must have non-empty state")]
    GenesisAssetMustHaveState,

    // ---- envelope / chain context ----------------------------------------
    /// `errNilTxID` — the tx id was never initialized.
    #[error("nil transaction ID")]
    NilTxId,
    /// `errTxNotCreateAsset` — expected a `CreateAssetTx`.
    #[error("transaction is not a CreateAssetTx")]
    TxNotCreateAsset,
    /// `errWrongNetworkID` — tx network id != this chain's.
    #[error("tx has wrong network ID")]
    WrongNetworkId,
    /// `errWrongChainID` / `errWrongBlockchainID`.
    #[error("tx has wrong blockchain ID")]
    WrongBlockchainId,

    // ---- state / diff (M5.10) --------------------------------------------
    /// `ErrMissingParentState` — a `Diff` could not resolve its parent state
    /// through the `Versions` resolver (specs 09 §5).
    #[error("missing parent state")]
    MissingParentState,

    /// A block was asked to `accept`/`reject` but it was never `verify`-ed, so no
    /// cached `Diff` exists for it (specs 09 §7).
    #[error("block state not found (block not verified)")]
    BlockNotVerified,

    // ---- block builder (M5.17) -------------------------------------------
    /// `ErrNoPendingBlocks` — the block builder was asked to build a block but
    /// there are no pending txs and no reason to advance the chain time (Go
    /// `builder.ErrNoPendingBlocks`, specs 09 §7.1).
    #[error("no pending blocks")]
    NoPendingBlocks,

    // ---- folded-in shared errors -----------------------------------------
    /// Linear-codec marshal/unmarshal failure.
    #[error(transparent)]
    Codec(#[from] ava_codec::error::CodecError),
    /// Shared fx / `verify` error (re-exported on `ava_vm::error::Error`).
    #[error(transparent)]
    Fx(#[from] ava_vm::error::Error),
    /// Database read/write failure (`database.ErrNotFound` / `ErrClosed` / other)
    /// from the state stores (M5.10, specs 09 §5).
    #[error(transparent)]
    Database(#[from] ava_database::error::Error),
}
