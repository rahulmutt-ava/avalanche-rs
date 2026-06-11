// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-chain wallet — atomic import/export builder / signer / backend (port of
//! `wallet/chain/c`).
//!
//! Only the atomic txs (X/P ↔ C) are built here; EVM account txs go through
//! reth's RPC (specs 12 §13).

use ava_types::id::Id;

pub mod backend;
pub mod builder;
pub mod signer;
pub mod wallet;

pub use backend::{Backend, WalletBackend};
pub use builder::{Builder, CBuilder};
pub use signer::{SignedTx, Signer};

/// The C-chain's alias (`c.Alias`).
pub const ALIAS: &str = "C";

/// `wallet/chain/c.Context` — chain configuration.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// `NetworkID`.
    pub network_id: u32,
    /// `BlockchainID` — the C-chain id.
    pub blockchain_id: Id,
    /// `AVAXAssetID`.
    pub avax_asset_id: Id,
}
