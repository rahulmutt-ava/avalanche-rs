// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Wallet SDK — P/X/C transaction builders, signers and backends (specs 12 §13).
//!
//! Port of `wallet/chain/{p,x,c}` + `wallet/subnet/primary/common`. Three layers
//! per chain:
//!
//! * **Builder** — selects UTXOs deterministically and constructs unsigned txs,
//!   byte-identical to the Go wallet (golden tx vectors, specs 12 §12.5).
//! * **Signer** — produces per-input secp256k1 credentials (and auth
//!   credentials) over a [`keychain::Keychain`].
//! * **Backend** — a *pure snapshot* of the UTXO set / owners; builders and
//!   signers do no I/O (specs 12 §13).
//!
//! The wallet facades + primary `make_wallet` (issue over the API) are M8.27 and
//! intentionally absent here.

#![forbid(unsafe_code)]

pub mod c;
pub mod common;
pub mod error;
pub mod keychain;
pub mod options;
pub mod p;
pub mod x;

pub use error::{Error, Result};
