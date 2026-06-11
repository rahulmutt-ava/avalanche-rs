// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-crate error enum (specs 12 §11, 00 §8).

/// Errors produced by the configuration subsystem.
///
/// Variants mirror the Go `config/` sentinel errors one-for-one where the Go
/// side has a named error; parse-shaped failures carry the offending key and
/// input so callers can render Go-equivalent messages.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A value failed to parse as a Go `time.ParseDuration` duration.
    #[error("time: invalid duration {input:?}")]
    InvalidDuration {
        /// The raw input string.
        input: String,
    },

    /// A duration string had a valid grammar but a missing unit suffix
    /// (Go: `time: missing unit in duration`).
    #[error("time: missing unit in duration {input:?}")]
    MissingDurationUnit {
        /// The raw input string.
        input: String,
    },

    /// A duration string had a valid grammar but an unknown unit suffix
    /// (Go: `time: unknown unit`).
    #[error("time: unknown unit {unit:?} in duration {input:?}")]
    UnknownDurationUnit {
        /// The unrecognized unit token.
        unit: String,
        /// The raw input string.
        input: String,
    },

    /// A negative duration. Valid in Go (`time.Duration` is signed), but
    /// `std::time::Duration` is unsigned; no avalanchego flag default is
    /// negative, and the durations are validated non-negative at parse time
    /// anyway (13 §6/§11/§12).
    #[error("negative duration {input:?} is not supported")]
    NegativeDurationUnsupported {
        /// The raw input string.
        input: String,
    },

    /// `--config-file-content` (or another `-content` flag) was not valid
    /// base64 (Go: `unable to decode base64 content: %w`).
    #[error("unable to decode base64 content: {msg}")]
    InvalidBase64Content {
        /// The underlying decode error.
        msg: String,
    },

    /// `--config-file-content-type` (or the config-file extension) was not
    /// one of json/yaml/toml (Go viper: `Unsupported Config Type %q`).
    #[error("unsupported config type {content_type:?}")]
    ConfigContentTypeNotSupported {
        /// The offending type string.
        content_type: String,
    },

    /// The `--config-file` path could not be read.
    #[error("unable to read config file {path:?}: {msg}")]
    ConfigFileRead {
        /// The offending path.
        path: String,
        /// The underlying I/O error.
        msg: String,
    },

    /// The config content failed to parse as its declared format.
    #[error("unable to parse {format} config content: {msg}")]
    ConfigParse {
        /// The format that was attempted (json/yaml/toml).
        format: String,
        /// The underlying parse error.
        msg: String,
    },

    /// The command line failed to parse (clap error rendered to text).
    #[error("{msg}")]
    CliParse {
        /// The rendered clap error.
        msg: String,
    },

    /// A key with no [`crate::flags::FlagSpec`] row was requested.
    #[error("unknown configuration key {key:?}")]
    UnknownKey {
        /// The offending key.
        key: String,
    },

    /// A layered value failed to parse as the flag's type.
    #[error("invalid value {value:?} for {key} (want {want}): {msg}")]
    InvalidFlagValue {
        /// The flag key.
        key: String,
        /// The offending raw value.
        value: String,
        /// The expected type name.
        want: &'static str,
        /// The underlying parse error.
        msg: String,
    },

    // -----------------------------------------------------------------------
    // get_node_config / subnet / chain-config sentinels (12 §1.6/§1.7, 13
    // §5/§7/§8/§13/§14/§18/§19; Go config/config.go + subnets/config.go).
    // -----------------------------------------------------------------------
    /// A resolved value failed a non-sentinel `fmt.Errorf` range/shape check
    /// (e.g. Go `"%s must be > 0"`). Carries the key and the Go-equivalent
    /// message tail.
    #[error("{key} {msg}")]
    InvalidValue {
        /// The flag key.
        key: String,
        /// The Go-message tail, e.g. `must be > 0`.
        msg: String,
    },

    /// `--network-id` was not a known name, `network-<n>`, or number
    /// (Go `constants.NetworkID`).
    #[error("failed to parse {name:?} as a network name")]
    InvalidNetworkId {
        /// The offending network name.
        name: String,
    },

    /// Supporting and objecting to the same ACP (Go `errConflictingACPOpinion`).
    #[error("supporting and objecting to the same ACP")]
    ConflictingACPOpinion,

    /// Objecting to an already-scheduled ACP
    /// (Go `errConflictingImplicitACPOpinion`).
    #[error("objecting to enabled ACP")]
    ConflictingImplicitACPOpinion,

    /// `--sybil-protection-enabled=false` with a zero disabled weight
    /// (Go `errSybilProtectionDisabledStakerWeights`).
    #[error("sybil protection disabled weights must be positive")]
    SybilProtectionDisabledStakerWeights,

    /// `--sybil-protection-enabled=false` on Mainnet/Fuji
    /// (Go `errSybilProtectionDisabledOnPublicNetwork`, 13 §5).
    #[error("sybil protection disabled on public network")]
    SybilProtectionDisabledOnPublicNetwork,

    /// `--uptime-requirement` outside `[0, 1]` (Go `errInvalidUptimeRequirement`).
    #[error("uptime requirement must be in the range [0, 1]")]
    InvalidUptimeRequirement,

    /// `--min-validator-stake` > `--max-validator-stake`
    /// (Go `errMinValidatorStakeAboveMax`).
    #[error("minimum validator stake can't be greater than maximum validator stake")]
    MinValidatorStakeAboveMax,

    /// `--min-delegation-fee` > 1,000,000 (Go `errInvalidDelegationFee`).
    #[error("delegation fee must be in the range [0, 1,000,000]")]
    InvalidDelegationFee,

    /// `--min-stake-duration` == 0 (Go `errInvalidMinStakeDuration`).
    #[error("min stake duration must be > 0")]
    InvalidMinStakeDuration,

    /// `--max-stake-duration` < `--min-stake-duration`
    /// (Go `errMinStakeDurationAboveMax`).
    #[error("max stake duration can't be less than min stake duration")]
    MinStakeDurationAboveMax,

    /// `--stake-max-consumption-rate` > `PercentDenominator`
    /// (Go `errStakeMaxConsumptionTooLarge`).
    #[error("max stake consumption must be less than or equal to 1000000")]
    StakeMaxConsumptionTooLarge,

    /// max consumption rate < min consumption rate
    /// (Go `errStakeMaxConsumptionBelowMin`).
    #[error("stake max consumption can't be less than min stake consumption")]
    StakeMaxConsumptionBelowMin,

    /// `--stake-minting-period` < `--max-stake-duration`
    /// (Go `errStakeMintingPeriodBelowMin`).
    #[error("stake minting period can't be less than max stake duration")]
    StakeMintingPeriodBelowMin,

    /// `--track-subnets` contained the Primary Network ID
    /// (Go `errCannotTrackPrimaryNetwork`).
    #[error("cannot track primary network")]
    CannotTrackPrimaryNetwork,

    /// `--staking-tls-cert-file-content` set without
    /// `--staking-tls-key-file-content` (Go `errStakingKeyContentUnset`).
    #[error("staking-tls-key-file-content key not set but staking-tls-cert-file-content set")]
    StakingKeyContentUnset,

    /// `--staking-tls-key-file-content` set without
    /// `--staking-tls-cert-file-content` (Go `errStakingCertContentUnset`).
    #[error("staking-tls-key-file-content key set but staking-tls-cert-file-content not set")]
    StakingCertContentUnset,

    /// More than one staking-signer option set (Go `errInvalidSignerConfig`).
    #[error(
        "only one of the following flags can be set: staking-ephemeral-signer-enabled, staking-signer-key-file-content, staking-signer-key-file, staking-rpc-signer-endpoint"
    )]
    InvalidSignerConfig,

    /// Loading/generating the staking TLS certificate failed.
    #[error("staking certificate: {msg}")]
    StakingCert {
        /// The underlying error.
        msg: String,
    },

    /// Both `--public-ip` and `--public-ip-resolution-service` were given
    /// (Go `getIPConfig`, 13 §19).
    #[error("only one of --public-ip and --public-ip-resolution-service can be given")]
    ConflictingPublicIpOptions,

    /// A disk-space percentage was outside `[0, 50]`
    /// (Go `errDiskSpaceOutOfRange`, 13 §18).
    #[error("out of range [0,50]: {key:?} ({value})")]
    DiskSpaceOutOfRange {
        /// The offending key.
        key: String,
        /// The offending value.
        value: u64,
    },

    /// Warning disk-space threshold below the fatal threshold
    /// (Go `errDiskWarnAfterFatal`).
    #[error(
        "warning disk space threshold cannot be greater than fatal threshold: {warn} < {required}"
    )]
    DiskWarnAfterFatal {
        /// The warning percentage.
        warn: u64,
        /// The required (fatal) percentage.
        required: u64,
    },

    /// One of `--bootstrap-ips`/`--bootstrap-ids` set without the other
    /// (Go `getBootstrapConfig`, 13 §13).
    #[error("set {set:?} but didn't set {unset:?}")]
    BootstrapMutuallyRequired {
        /// The key that was set.
        set: String,
        /// The key that was not.
        unset: String,
    },

    /// `--bootstrap-ips`/`--bootstrap-ids` counts differ (13 §13).
    #[error(
        "expected the number of bootstrapIPs ({ips}) to match the number of bootstrapIDs ({ids})"
    )]
    BootstrapPeerCountMismatch {
        /// The number of IPs.
        ips: usize,
        /// The number of IDs.
        ids: usize,
    },

    /// `--state-sync-ips`/`--state-sync-ids` counts differ (13 §13).
    #[error(
        "expected the number of stateSyncIPs ({ips}) to match the number of stateSyncIDs ({ids})"
    )]
    StateSyncPeerCountMismatch {
        /// The number of IPs.
        ips: usize,
        /// The number of IDs.
        ids: usize,
    },

    /// A custom upgrade file/content was supplied on a standard network
    /// (Go `getUpgradeConfig`, 13 §21).
    #[error("cannot configure upgrades for networkID: {network}")]
    UpgradeNotAllowed {
        /// The network name.
        network: String,
    },

    /// A genesis build/validate failure (ava-genesis, 13 §21).
    #[error(transparent)]
    Genesis(#[from] ava_genesis::GenesisError),

    /// More than one of `consensusParameters`/`snowParameters`/
    /// `simplexParameters` in a subnet config
    /// (Go `subnets.ErrTooManyConsensusParameters`, 13 §14).
    #[error("only one of consensusParameters, snowParameters, or simplexParameters can be set")]
    TooManyConsensusParameters,

    /// `allowedNodes` non-empty while `validatorOnly=false`
    /// (Go `errAllowedNodesWhenNotValidatorOnly`).
    #[error("allowedNodes can only be set when ValidatorOnly is true")]
    AllowedNodesWhenNotValidatorOnly,

    /// Snow/simplex parameters failed `Verify()` (or neither was set after
    /// defaulting; Go `errNoParametersSet` / `Parameters.Verify`).
    #[error("invalid consensus parameters: {msg}")]
    InvalidConsensusParameters {
        /// The verify failure text.
        msg: String,
    },

    /// JSON unmarshalling failed (Go `errUnmarshalling`).
    #[error("unmarshalling failed on {what}: {msg}")]
    Unmarshalling {
        /// What was being unmarshalled.
        what: String,
        /// The underlying parse error.
        msg: String,
    },

    /// An explicitly-configured config dir does not exist
    /// (Go `errCannotReadDirectory`, 13 §14).
    #[error("cannot read directory: {path}")]
    CannotReadDirectory {
        /// The cleaned, expanded path.
        path: String,
    },

    /// An explicitly-configured file does not exist
    /// (Go `errFileDoesNotExist`).
    #[error("file does not exist: {path}")]
    FileDoesNotExist {
        /// The cleaned, expanded path.
        path: String,
    },
}

/// Crate-local result alias.
pub type Result<T, E = ConfigError> = std::result::Result<T, E>;
