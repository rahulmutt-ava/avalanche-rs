// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-chain wallet — builder / signer / backend (port of `wallet/chain/x`).

use ava_types::id::Id;

pub mod backend;
pub mod builder;
pub mod signer;

pub use backend::{Backend, WalletBackend};
pub use builder::{Builder, XBuilder};
pub use signer::{SignedTx, Signer};

/// The X-chain's alias (`builder.Alias`).
pub const ALIAS: &str = "X";

/// `builder.SECP256K1FxIndex`.
pub const SECP256K1_FX_INDEX: u32 = 0;

/// `wallet/chain/x/builder.Context` — chain configuration + static fees.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// `NetworkID`.
    pub network_id: u32,
    /// `BlockchainID` — the X-chain id.
    pub blockchain_id: Id,
    /// `AVAXAssetID`.
    pub avax_asset_id: Id,
    /// `BaseTxFee` — the static fee for every non-create-asset tx.
    pub base_tx_fee: u64,
    /// `CreateAssetTxFee`.
    pub create_asset_tx_fee: u64,
}
