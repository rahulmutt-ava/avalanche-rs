// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-crate error enum (specs 00 §7.4).
//!
//! Mirrors the sentinel errors of `wallet/chain/p/builder` /
//! `wallet/chain/x/builder` / `wallet/chain/c` plus the local codec/crypto
//! plumbing failures.

use ava_types::id::Id;

/// Wallet errors (build/sign failures over a backend snapshot).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Go `ErrNoChangeAddress` — the address set is empty.
    #[error("no possible change address")]
    NoChangeAddress,
    /// Go `ErrUnknownOutputType`.
    #[error("unknown output type")]
    UnknownOutputType,
    /// Go `ErrUnknownOwnerType`.
    #[error("unknown owner type")]
    UnknownOwnerType,
    /// Go `ErrInsufficientAuthorization` — the keychain cannot meet the owner
    /// threshold for a subnet/validator authorization.
    #[error("insufficient authorization")]
    InsufficientAuthorization,
    /// Go `ErrInsufficientFunds` — the UTXO set cannot cover `amount + fee`.
    #[error("insufficient funds: needs {amount} more units of asset {asset_id}")]
    InsufficientFunds {
        /// The shortfall.
        amount: u64,
        /// The asset that is short.
        asset_id: Id,
    },
    /// Go `errInsufficientFunds: no UTXOs available to import`.
    #[error("insufficient funds: no UTXOs available to import")]
    NoImportableFunds,
    /// Go `ErrInvalidUTXOSigIndex` — a sig index points past the owner set.
    #[error("invalid UTXO signature index")]
    InvalidUtxoSigIndex,
    /// Go `ErrUnsupportedTxType` — the signer cannot sign this tx kind.
    #[error("unsupported tx type")]
    UnsupportedTxType,
    /// Checked arithmetic overflowed (Go `safemath` error paths).
    #[error("arithmetic overflow")]
    Overflow,
    /// A UTXO referenced by the tx is missing from the backend snapshot when a
    /// full signature set was required.
    #[error("missing UTXO {utxo_id} on chain {chain_id}")]
    MissingUtxo {
        /// The chain the UTXO was expected on.
        chain_id: Id,
        /// The UTXO id.
        utxo_id: Id,
    },
    /// The owner for an authorization is missing from the backend snapshot.
    #[error("missing owner {0}")]
    MissingOwner(Id),
    /// Codec (de)serialization failure.
    #[error("codec: {0}")]
    Codec(#[from] ava_codec::error::CodecError),
    /// secp256k1 signing failure.
    #[error("crypto: {0}")]
    Crypto(#[from] ava_crypto::error::Error),
}

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
