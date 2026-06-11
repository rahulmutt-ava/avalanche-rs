// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Narrow API-client traits — the seam the wallet facades issue over and
//! [`crate::primary::make_wallet`] fetches state through (specs 12 §13).
//!
//! Mirrors exactly the client surface Go's `wallet/subnet/primary` consumes:
//! `info.Client` (network id, blockchain-id discovery), `platformvm.Client`
//! (issue / poll / `GetAtomicUTXOs` / `GetOwners` / fee+asset context),
//! `avm.Client` (issue / poll / `GetAtomicUTXOs` / `GetAssetDescription` /
//! `GetTxFee`), the C-chain avax client (issue / poll / `GetAtomicUTXOs`) and
//! `ethclient.Client` (`BalanceAt` / `NonceAt` / `EstimateBaseFee`).
//!
//! TODO(M8.18/M8.22): the live JSON-RPC-over-HTTP implementations land with
//! the `ava-api` client tasks; until then tests provide in-memory mocks and
//! `make_wallet` takes the trait objects instead of a `uri` (the
//! deferred-live-transport pattern; see `tests/PORTING.md`).
//!
//! `GetAtomicUTXOs` paging (Go's `fetchLimit = 1024` loop in `AddAllUTXOs`)
//! is a transport concern: implementations return the *complete* UTXO set,
//! paging internally.

use std::collections::BTreeMap;

use async_trait::async_trait;
use ava_platformvm::txs::fee::gas::Dimensions;
use ava_secp256k1fx::OutputOwners;
use ava_types::id::Id;
use ava_types::short_id::ShortId;

use crate::error::Result;

/// `info.Client` — the subset `make_wallet` needs.
#[async_trait]
pub trait InfoClient: Send + Sync {
    /// `info.getNetworkID`.
    async fn get_network_id(&self) -> Result<u32>;

    /// `info.getBlockchainID(alias)` — resolves "X" / "C".
    async fn get_blockchain_id(&self, alias: &str) -> Result<Id>;
}

/// `platformvm.Client` — the subset the P wallet + `make_wallet` need.
#[async_trait]
pub trait PChainClient: Send + Sync {
    /// `platform.issueTx` — submits the signed tx bytes, returning the tx id.
    async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id>;

    /// `AwaitTxAccepted` — polls until the tx is decided (poll frequency is an
    /// implementation concern; Go defaults to 100ms).
    async fn await_tx_accepted(&self, tx_id: Id) -> Result<()>;

    /// `platform.getAtomicUTXOs` — every UTXO referencing `addrs` exported
    /// from `source_chain_id` to the P-chain (the P-chain's own id returns the
    /// local UTXOs), as canonical codec bytes. Complete set (paged internally).
    async fn get_atomic_utxos(
        &self,
        addrs: &[ShortId],
        source_chain_id: Id,
    ) -> Result<Vec<Vec<u8>>>;

    /// `platformvm.Client.GetOwners` — the owners of the given subnets, L1
    /// validations and auto-renewed validator txs (`WalletConfig`).
    async fn get_owners(
        &self,
        subnet_ids: &[Id],
        validation_ids: &[Id],
        auto_renewed_validator_tx_ids: &[Id],
    ) -> Result<BTreeMap<Id, OutputOwners>>;

    /// `platform.getStakingAssetID` (primary network) — the AVAX asset id.
    async fn get_staking_asset_id(&self) -> Result<Id>;

    /// `platform.getFeeConfig` — the ACP-103 complexity weights.
    async fn get_dynamic_fee_weights(&self) -> Result<Dimensions>;

    /// `platform.getFeeState` — the current gas price (the wallet context
    /// doubles it; `wallet/chain/p/context.go`).
    async fn get_gas_price(&self) -> Result<u64>;
}

/// `avm.Client` — the subset the X wallet + `make_wallet` need.
#[async_trait]
pub trait XChainClient: Send + Sync {
    /// `avm.issueTx` — submits the signed tx bytes, returning the tx id.
    async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id>;

    /// `AwaitTxAccepted` — polls until the tx is decided.
    async fn await_tx_accepted(&self, tx_id: Id) -> Result<()>;

    /// `avm.getUTXOs(sourceChain)` — every UTXO referencing `addrs` exported
    /// from `source_chain_id` to the X-chain, as canonical codec bytes.
    async fn get_atomic_utxos(
        &self,
        addrs: &[ShortId],
        source_chain_id: Id,
    ) -> Result<Vec<Vec<u8>>>;

    /// `avm.getAssetDescription("AVAX")` — the AVAX asset id.
    async fn get_avax_asset_id(&self) -> Result<Id>;

    /// `avm.getTxFee` — `(base_tx_fee, create_asset_tx_fee)` in nAVAX.
    async fn get_tx_fees(&self) -> Result<(u64, u64)>;
}

/// The C-chain avax (atomic) API client (`graft/coreth plugin/evm/client`) —
/// the subset the C wallet + `make_wallet` need.
#[async_trait]
pub trait CChainClient: Send + Sync {
    /// `avax.issueTx` — submits the signed atomic tx bytes, returning the tx
    /// id.
    async fn issue_tx(&self, tx_bytes: &[u8]) -> Result<Id>;

    /// `AwaitTxAccepted` — polls until the tx is decided.
    async fn await_tx_accepted(&self, tx_id: Id) -> Result<()>;

    /// `avax.getUTXOs(sourceChain)` — every atomic UTXO referencing `addrs`
    /// exported from `source_chain_id` to the C-chain, as canonical codec
    /// bytes.
    async fn get_atomic_utxos(
        &self,
        addrs: &[ShortId],
        source_chain_id: Id,
    ) -> Result<Vec<Vec<u8>>>;
}

/// `ethclient.Client` — the subset `make_wallet` (account state) and the C
/// wallet facade (base-fee estimation) need.
#[async_trait]
pub trait EthClient: Send + Sync {
    /// `BalanceAt(addr, nil)` — the latest balance in wei.
    async fn balance(&self, addr: &[u8; 20]) -> Result<u128>;

    /// `NonceAt(addr, nil)` — the latest nonce.
    async fn nonce(&self, addr: &[u8; 20]) -> Result<u64>;

    /// `EstimateBaseFee` — the suggested base fee in wei (the C facade's
    /// default when `WithBaseFee` is not given).
    async fn estimate_base_fee(&self) -> Result<u128>;
}
