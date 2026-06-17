// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The ACP-118 sign-decision for the SAE C-Chain (Go `cchain/warp/verifier.go`).
//!
//! [`Verifier`] decides whether this node should sign an inbound warp
//! [`UnsignedMessage`]. A node signs a message iff:
//!
//! 1. the message is in [`Storage`] (precompile-emitted or off-chain), OR
//! 2. its payload is a [`payload::Hash`](ava_warp::payload::Hash)
//!    block-attestation whose block the [`Backend`] reports accepted.
//!
//! Every refusal carries one of four [`AppErrorCode`]s
//! ([`AppErrorCode::Storage`]/[`AppErrorCode::Parse`]/[`AppErrorCode::Unknown`]/
//! [`AppErrorCode::NotAccepted`], values `1`/`2`/`3`/`4`), reproducing Go's
//! `iota+1` constants for p2p `AppError` parity.

use ava_database::Error as DbError;
use ava_database::traits::Database;
use ava_types::id::Id;
use ava_warp::UnsignedMessage;
use ava_warp::payload::WarpPayload;

use super::Error;
use super::storage::Storage;

/// The ACP-118 refusal codes returned by [`Verifier::verify`] to identify why a
/// message was not signed (Go `cchain/warp/verifier.go`, `iota+1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AppErrorCode {
    /// `StorageErrCode` (1) — the storage lookup itself failed (not a miss).
    Storage = 1,
    /// `ParseErrCode` (2) — the message payload failed to parse.
    Parse = 2,
    /// `UnknownMessageErrCode` (3) — the payload parsed but is not a block hash.
    Unknown = 3,
    /// `NotAcceptedErrCode` (4) — the attested block is not accepted.
    NotAccepted = 4,
}

impl AppErrorCode {
    /// The numeric `AppError.Code` (Go `iota+1`): `1`/`2`/`3`/`4`.
    #[must_use]
    pub fn code(self) -> u32 {
        self as u32
    }
}

/// A refusal to sign — the SAE analog of Go's `*common.AppError` (a code plus a
/// human-readable message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppError {
    /// The ACP-118 refusal code (`1`..=`4`).
    pub code: AppErrorCode,
    /// A human-readable description of why the message was not signed.
    pub message: String,
}

impl AppError {
    /// Builds an [`AppError`] with `code` and `message`.
    fn new(code: AppErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The `Backend` the [`Verifier`] depends on to look up accepted blocks
/// (Go `cchain/warp/verifier.go`).
///
/// Modeled as a trait so tests can stub it (the production VM backs it with the
/// SAE block index).
pub trait Backend {
    /// `IsAccepted(blockID)` — whether the block with `block_id` is accepted.
    ///
    /// Go returns a non-nil error when the block is not accepted; the Rust
    /// analog returns a `bool` (the [`Verifier`] turns `false` into a
    /// [`AppErrorCode::NotAccepted`] refusal).
    fn is_accepted(&self, block_id: Id) -> bool;
}

/// `Verifier` decides whether this node should sign a warp message
/// (Go `cchain/warp/verifier.go`, an `acp118.Verifier`).
pub struct Verifier<'a, B: Backend, D: Database> {
    /// The accepted-block oracle.
    backend: B,
    /// The warp message store (precompile-emitted + off-chain messages).
    storage: &'a Storage<D>,
}

impl<'a, B: Backend, D: Database> Verifier<'a, B, D> {
    /// `NewVerifier(backend, storage)` — an ACP-118 message verifier.
    pub fn new(backend: B, storage: &'a Storage<D>) -> Self {
        Self { backend, storage }
    }

    /// `Verify(m)` — whether this node should sign `m`.
    ///
    /// Returns `Ok(())` to sign, or an [`AppError`] (carrying one of the four
    /// [`AppErrorCode`]s) to refuse.
    ///
    /// # Errors
    /// Returns an [`AppError`] when the message is neither known to [`Storage`]
    /// nor a block-hash attestation of an accepted block. The four refusal codes
    /// mirror Go exactly (see [`AppErrorCode`]).
    pub fn verify(&self, m: &UnsignedMessage) -> Result<(), AppError> {
        // If the message was sent by the precompile or registered as an off-chain
        // message, it will be available in storage.
        let id = m
            .id()
            .map_err(|e| AppError::new(AppErrorCode::Storage, format!("computing id: {e}")))?;
        match self.storage.get(id) {
            Ok(_) => return Ok(()),
            Err(Error::Db(DbError::NotFound)) => {}
            Err(e) => {
                return Err(AppError::new(
                    AppErrorCode::Storage,
                    format!("loading message: {e}"),
                ));
            }
        }

        // Block acceptance doesn't go through the precompile, so we need to check
        // whether the message is for an accepted block.
        let payload = WarpPayload::parse(&m.payload)
            .map_err(|e| AppError::new(AppErrorCode::Parse, format!("parsing payload: {e}")))?;

        let WarpPayload::Hash(hash) = payload else {
            return Err(AppError::new(
                AppErrorCode::Unknown,
                "unknown message".to_string(),
            ));
        };

        if !self.backend.is_accepted(hash.hash) {
            return Err(AppError::new(
                AppErrorCode::NotAccepted,
                "block not marked as accepted".to_string(),
            ));
        }
        Ok(())
    }
}
