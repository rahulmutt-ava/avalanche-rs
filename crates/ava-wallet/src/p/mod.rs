// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-chain wallet — builder / signer / backend (port of `wallet/chain/p`).

use ava_platformvm::txs::fee::gas::Dimensions;
use ava_types::id::Id;

pub mod backend;
pub mod builder;
pub mod fee;
pub mod signer;
pub mod wallet;

pub use backend::{Backend, WalletBackend};
pub use builder::{Builder, PBuilder};
pub use signer::{SignedTx, Signer};

/// The P-chain's alias (`builder.Alias`).
pub const ALIAS: &str = "P";

/// `constants.PlatformChainID` — `ids.Empty`.
pub const PLATFORM_CHAIN_ID: Id = Id::EMPTY;

/// `wallet/chain/p/builder.Context` — the chain configuration a builder prices
/// and stamps txs with.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// `NetworkID`.
    pub network_id: u32,
    /// `AVAXAssetID`.
    pub avax_asset_id: Id,
    /// `ComplexityWeights` — the ACP-103 gas weights.
    pub complexity_weights: Dimensions,
    /// `GasPrice`.
    pub gas_price: u64,
}
