// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The crate error model (specs 07 §9).
//!
//! A single `thiserror` enum carrying the Go sentinels that callers assert via
//! `matches!` (mirroring `errors.Is`). Three families are preserved:
//!
//! * the database/lookup sentinel ([`Error::NotFound`] ⇐ `database.ErrNotFound`),
//! * the rpcchainvm host/guest sentinels ([`Error::RemoteVmNotImplemented`],
//!   [`Error::StateSyncableVmNotImplemented`], [`Error::ProtocolVersionMismatch`],
//!   [`Error::HandshakeFailed`], [`Error::ProcessNotFound`]), and
//! * the fx wrong-type / verification set that `ava-secp256k1fx` re-exports.
//!
//! [`AppError`](crate::app::AppError) is a **separate** typed error (`i32` code,
//! `Is`-by-code) defined in [`crate::app`]; it is not a variant here.

use std::fmt::Debug;

/// The crate-wide result alias (specs 00 §7.1).
pub type Result<T> = std::result::Result<T, Error>;

/// VM-framework errors (specs 07 §9).
///
/// Variants are matched structurally with `matches!`/`assert_matches!`, which is
/// the Rust analogue of Go's `errors.Is(err, ErrFoo)` sentinel checks.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    // ---- database / lookup sentinel ----
    /// `database.ErrNotFound` — a block/height/state-summary lookup missed.
    #[error("not found")]
    NotFound,

    // ---- rpcchainvm host/guest sentinels ----
    /// `ErrRemoteVMNotImplemented` — the remote VM does not implement the
    /// optional capability the caller probed (e.g. the batched fallback).
    #[error("vm does not implement RemoteVM interface")]
    RemoteVmNotImplemented,

    /// The VM does not implement the optional `StateSyncableVM` capability.
    #[error("vm does not implement StateSyncableVM interface")]
    StateSyncableVmNotImplemented,

    /// rpcchainvm handshake: the plugin reported an incompatible protocol
    /// version.
    #[error("protocol version mismatch")]
    ProtocolVersionMismatch,

    /// rpcchainvm handshake failed (timeout or transport error).
    #[error("handshake failed")]
    HandshakeFailed,

    /// rpcchainvm runtime: the plugin process could not be located.
    #[error("process not found")]
    ProcessNotFound,

    // ---- fx wrong-type / verification set (re-exported by ava-secp256k1fx) ----
    /// The fx was initialized against an unexpected host-VM type.
    #[error("wrong vm type")]
    WrongVmType,
    /// The transaction passed to the fx had an unexpected type.
    #[error("wrong tx type")]
    WrongTxType,
    /// The input passed to the fx had an unexpected type.
    #[error("wrong input type")]
    WrongInputType,
    /// The credential passed to the fx had an unexpected type.
    #[error("wrong credential type")]
    WrongCredentialType,
    /// The owner passed to the fx had an unexpected type.
    #[error("wrong owner type")]
    WrongOwnerType,
    /// The UTXO passed to the fx had an unexpected type.
    #[error("wrong utxo type")]
    WrongUtxoType,
    /// The operation passed to the fx had an unexpected type.
    #[error("wrong operation type")]
    WrongOpType,
    /// Produced and consumed amounts did not balance.
    #[error("mismatched amounts")]
    MismatchedAmounts,
    /// The operation referenced the wrong number of UTXOs.
    #[error("wrong number of UTXOs for the operation")]
    WrongNumberOfUtxos,
    /// A mint operation created an unexpected output.
    #[error("wrong mint created")]
    WrongMintCreated,
    /// The output is still timelocked and cannot be spent.
    #[error("output is timelocked")]
    Timelocked,
    /// The credential supplied more signatures than the threshold allows.
    #[error("too many signers")]
    TooManySigners,
    /// The credential supplied fewer signatures than the threshold requires.
    #[error("too few signers")]
    TooFewSigners,
    /// A signature index referenced an address outside the owner set.
    #[error("input output index out of bounds")]
    InputOutputIndexOutOfBounds,
    /// The number of input signers did not match the credential.
    #[error("input credential signers mismatch")]
    InputCredentialSignersMismatch,
    /// A signature did not recover to the expected address.
    #[error("wrong signature")]
    WrongSig,
    /// The output owners are unspendable (e.g. zero threshold with no addrs).
    #[error("output is unspendable")]
    OutputUnspendable,
    /// The output is spendable but not in its optimal/canonical form.
    #[error("output representation should be optimized")]
    OutputUnoptimized,
    /// Addresses were not sorted-and-unique as the codec requires.
    #[error("addresses not sorted and unique")]
    AddrsNotSortedUnique,
}
