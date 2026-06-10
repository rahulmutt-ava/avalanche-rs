// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`GenesisError`] — one variant per Go sentinel error so `require.ErrorIs`
//! parity tests map 1:1 (specs 23 §6.1).

/// Per-crate result alias.
pub type Result<T> = std::result::Result<T, GenesisError>;

/// Genesis construction/validation errors. Each variant mirrors a Go sentinel
/// (`genesis/genesis.go`, `genesis/config.go`, `genesis/unparsed_config.go`,
/// `vms/platformvm/genesis/genesis.go`); the messages are the Go error strings.
#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    /// `errConflictingNetworkIDs`.
    #[error("conflicting networkIDs: expected {expected} but config contains {actual}")]
    ConflictingNetworkIds {
        /// The network id the node was started with.
        expected: u32,
        /// The network id found in the genesis config.
        actual: u32,
    },
    /// `errNoSupply`.
    #[error("initial supply must be > 0")]
    NoSupply,
    /// `errFutureStartTime`.
    #[error("startTime cannot be in the future: {0}")]
    FutureStartTime(u64),
    /// `errNoStakeDuration`.
    #[error("initial stake duration must be > 0")]
    NoStakeDuration,
    /// `errStakeDurationTooHigh`.
    #[error("initial stake duration larger than maximum configured")]
    StakeDurationTooHigh,
    /// `errNoStakers`.
    #[error("initial stakers must be > 0")]
    NoStakers,
    /// `errInitialStakeDurationTooLow`.
    #[error("initial stake duration is too low must be at least {0}")]
    InitialStakeDurationTooLow(u64),
    /// `errNoInitiallyStakedFunds`.
    #[error("initial staked funds cannot be empty")]
    NoInitiallyStakedFunds,
    /// `errDuplicateInitiallyStakedAddress`.
    #[error("duplicate initially staked address: {0}")]
    DuplicateInitiallyStakedAddress(String),
    /// `errNoAllocationToStake`.
    #[error("no allocation to stake in address {0}")]
    NoAllocationToStake(String),
    /// `errAllocationsLockedAmountTooLow`.
    #[error("total allocations locked amount is too low: {locked} locked < {stakers} stakers")]
    AllocationsLockedAmountTooLow {
        /// Sum of all `unlock_schedule[].amount`.
        locked: u64,
        /// Number of initial stakers.
        stakers: u64,
    },
    /// `errNoCChainGenesis`.
    #[error("C-Chain genesis cannot be empty")]
    NoCChainGenesis,
    /// `errOverridesStandardNetworkConfig`.
    #[error("overrides standard network genesis config: {0}")]
    OverridesStandardNetworkConfig(String),
    /// `errInvalidGenesisJSON`.
    #[error("could not unmarshal genesis JSON: {0}")]
    InvalidGenesisJson(String),
    /// `errNoTxs` (`AVAXAssetID` over an empty AVM genesis).
    #[error("genesis creates no transactions")]
    NoTxs,
    /// `errUTXOHasNoValue` (`platformvm/genesis.New`).
    #[error("genesis UTXO has no value")]
    UtxoHasNoValue,
    /// `errValidatorHasNoWeight` (`platformvm/genesis.New`).
    #[error("validator has not weight")]
    ValidatorHasNoWeight,
    /// `errValidatorAlreadyExited` (`platformvm/genesis.New`).
    #[error("validator would have already unstaked")]
    ValidatorAlreadyExited,
    /// `errStakeOverflow` (`platformvm/genesis.New`).
    #[error("validator stake exceeds limit")]
    StakeOverflow,
    /// `errInvalidETHAddress` (`unparsed_config.go`).
    #[error("invalid eth address")]
    InvalidEthAddress,
    /// `math.Add` overflow computing the initial supply (`config.InitialSupply`).
    #[error("initial supply calculation overflowed")]
    SupplyOverflow,
    /// Unix-time arithmetic overflowed building the staking window.
    #[error("staking time computation overflowed")]
    TimeOverflow,
    /// `VMGenesis` found no `CreateChainTx` with the requested VM id.
    #[error("couldn't find blockchain with VM ID {0}")]
    UnknownVmId(ava_types::id::Id),
    /// A linear-codec marshal/unmarshal failure.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),
    /// An `ava-platformvm` genesis marshal/parse failure.
    #[error("platformvm genesis: {0}")]
    Platform(#[from] ava_platformvm::error::Error),
    /// A bech32/hex address parse/format failure.
    #[error("address: {0}")]
    Address(#[from] ava_crypto::error::Error),
    /// An `ava-types` primitive failure (bad id/short-id length, …).
    #[error("types: {0}")]
    Types(#[from] ava_types::error::Error),
    /// A filesystem failure loading a custom genesis config (`GetConfigFile`).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A base64 decode failure loading `--genesis-file-content` (`GetConfigContent`).
    #[error("unable to decode base64 content: {0}")]
    InvalidBase64(String),
}
