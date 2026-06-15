// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-node` error enum for node assembly (`Node::new`, specs/12 §2.2).
//!
//! Each variant mirrors the corresponding `fmt.Errorf` wrap in Go
//! `node/node.go::New` so the failure surface stays recognizable step-by-step.

use ava_types::id::Id;

/// Errors raised while assembling the node (mirror the per-step error wraps of
/// Go `node.New`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Step 1: the staking TLS certificate failed strict parsing
    /// (Go `"invalid staking certificate: %w"`).
    #[error("invalid staking certificate: {0}")]
    StakingCert(String),

    /// Step 2: the BLS staking signer could not be built
    /// (Go `"problem initializing staking signer: %w"`).
    #[error("problem initializing staking signer: {0}")]
    StakingSigner(String),

    /// Step 2: the configured RPC remote signer
    /// (`--staking-rpc-signer-endpoint`) is not yet supported by the Rust node
    /// (deferral documented in `tests/PORTING.md`).
    #[error("problem initializing staking signer: rpc signer is not supported yet: {0}")]
    RpcSignerUnsupported(String),

    /// Step 4 / 19: a VM alias could not be registered.
    #[error("couldn't initialize API aliases: {0}")]
    VmAlias(#[from] ava_chains::error::Error),

    /// Step 5: a bootstrap beacon could not be added
    /// (Go `"problem initializing node beacons: %w"`).
    #[error("problem initializing node beacons: {0}")]
    Bootstrappers(String),

    /// Step 6: the OpenTelemetry tracer could not be built
    /// (Go `"couldn't initialize tracer: %w"`).
    #[error("couldn't initialize tracer: {0}")]
    Tracer(#[from] crate::trace::TraceError),

    /// Steps 7/10/11/13/15/18/20/21: a metrics namespace could not be
    /// registered on the node gatherer
    /// (Go `"couldn't initialize metrics: %w"` and the bare registrations).
    #[error("couldn't initialize metrics: {0}")]
    Metrics(#[from] ava_api::metrics::MetricsError),

    /// Step 9/10/18/22/23/24: an API route / alias could not be mounted
    /// (Go `"couldn't initialize API server: %w"` et al.).
    #[error("couldn't initialize API server: {0}")]
    ApiServer(#[from] ava_api::error::ApiError),

    /// Step 11: the database could not be opened
    /// (Go `"problem initializing database: %w"`).
    #[error("problem initializing database: {0}")]
    Database(#[from] ava_database::Error),

    /// Step 11: an unusable database configuration (an on-disk backend without
    /// the `rocksdb` feature, or a corrupt persisted genesis-hash record).
    #[error("problem initializing database: {0}")]
    DatabaseInit(String),

    /// Step 11 (pre-open): the configured data directory was written by a Go
    /// node (or a prior `PREV_DATABASE` schema) in a backend the Rust node
    /// cannot open in place (Pebble/goleveldb, 04 §11 / 26 §6). The node
    /// **refuses to start** rather than silently corrupting it; run the offline
    /// import tool first or bootstrap fresh from the network (04 §11.5).
    #[error(
        "refusing to open foreign data directory {path}: detected a {backend} \
         schema-version folder the Rust node cannot open in place. Run the \
         offline import tool (`avalanchers db migrate`, 04 §11) to migrate it to \
         RocksDB ({current}), or bootstrap fresh from the network (04 §11.5)."
    )]
    ForeignDataDir {
        /// The data directory that was refused.
        path: std::path::PathBuf,
        /// The foreign backend / schema folder detected (e.g. `pebble`,
        /// `v1.0.0`).
        backend: String,
        /// The RocksDB schema-version folder the node would have opened
        /// (`CURRENT_DATABASE`).
        current: &'static str,
    },

    /// Step 11: the persisted genesis hash does not match the configured
    /// genesis (byte-stable Go message).
    #[error(
        "db contains invalid genesis hash. DB Genesis: {db_genesis} Generated Genesis: {expected_genesis}"
    )]
    GenesisHashMismatch {
        /// The genesis hash found in the database.
        db_genesis: Id,
        /// The genesis hash computed from the configured genesis bytes.
        expected_genesis: Id,
    },

    /// Step 13: the configured compression type is unknown
    /// (`--network-compression-type`).
    #[error("problem initializing message creator: unknown compression type: {0:?}")]
    UnknownCompressionType(String),

    /// Step 16: networking could not be initialized
    /// (Go `"problem initializing networking: %w"`).
    #[error("problem initializing networking: {0}")]
    Networking(String),

    /// Step 16: the configured public-IP resolution service has no concrete
    /// resolver yet (deferral documented in `tests/PORTING.md`).
    #[error("couldn't create IP resolver: unsupported resolution service: {0}")]
    UnsupportedResolver(String),

    /// Step 18: the health service could not be built
    /// (Go `"couldn't initialize health API: %w"`).
    #[error("couldn't initialize health API: {0}")]
    Health(#[from] ava_api::health::HealthError),

    /// Step 20: the adaptive timeout manager could not be built
    /// (Go `"couldn't initialize chain manager: %w"`).
    #[error("couldn't initialize chain manager: {0}")]
    ChainManager(String),

    /// Step 23: the genesis bytes could not be parsed for chain/API aliases.
    #[error("couldn't initialize chain aliases: {0}")]
    ChainAliases(#[from] ava_genesis::GenesisError),

    /// Step 23: a default or configured chain alias could not be registered
    /// (Go `"couldn't initialize chain aliases: %w"`). Not `#[from]`: the same
    /// source type maps to [`Error::VmAlias`] in steps 4/19.
    #[error("couldn't initialize chain aliases: {0}")]
    ChainAlias(ava_chains::error::Error),

    /// Step 24: the indexer could not be created
    /// (Go `"couldn't create index for txs: %w"`).
    #[error("couldn't create index for txs: {0}")]
    Indexer(#[from] ava_indexer::error::Error),

    /// A blocking helper task (NAT probe / DNS lookup) was cancelled or
    /// panicked before producing a result.
    #[error("blocking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),

    /// A bind / socket-level I/O failure (HTTP and staking listeners).
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for node assembly.
pub type Result<T> = std::result::Result<T, Error>;
