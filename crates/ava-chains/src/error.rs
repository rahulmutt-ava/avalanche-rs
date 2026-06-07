// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-chains` error model (specs 07 §9). Per-crate `thiserror` enum with
//! the preserved Go sentinels (`database.ErrNotFound` ⇒ [`Error::NotFound`]) and
//! the chain-manager registration sentinels.

use ava_types::id::Id;

/// `ava-chains` result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors raised by the chain manager, registry, aliaser, subnet, and atomic
/// shared memory.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A factory / alias / chain / atomic key was not found
    /// (`database.ErrNotFound` parity — asserted via `matches!`).
    #[error("not found")]
    NotFound,

    /// A VM id already has a registered factory (Go `errDuplicatedVM`).
    #[error("there is already a VM registered with that ID")]
    VmAlreadyRegistered,

    /// An alias is already mapped to a (different) chain id (Go
    /// `ids.Aliaser.Alias` collision).
    #[error("alias {alias} is already in use")]
    AliasAlreadyInUse {
        /// The alias that collided.
        alias: String,
    },

    /// Creating a non-P-Chain with the P-Chain VM id (Go `errCreatePlatformVM`).
    #[error("attempted to create a chain running the PlatformVM outside the primary network")]
    CreatePlatformVm,

    /// The subnet has no consensus parameters configured (Go sanity check in
    /// `createSnowmanChain`).
    #[error("snowball parameters not specified for subnet {subnet}")]
    MissingSnowParameters {
        /// The subnet missing parameters.
        subnet: Id,
    },

    /// A duplicate put/remove against the same atomic key (Go
    /// `errDuplicatePut`/`errDuplicateRemove`).
    #[error("duplicate atomic operation on key")]
    DuplicateAtomicOp,

    /// An error from the underlying storage layer (specs 04).
    #[error("database error: {0}")]
    Database(#[from] ava_database::Error),

    /// An error from the consensus / VM stack while building a chain.
    #[error("vm error: {0}")]
    Vm(#[from] ava_vm::Error),

    /// A catch-all for context the variants above do not capture.
    #[error("{0}")]
    Other(String),
}
