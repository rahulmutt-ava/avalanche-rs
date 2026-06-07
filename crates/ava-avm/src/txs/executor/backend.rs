// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain tx executor [`Backend`] + minimal [`Config`] (specs 09 §6).
//!
//! Port of `vms/avm/txs/executor/backend.go` (`Backend`) plus the subset of
//! `vms/avm/config.Config` the verifier consumes. Go's `Backend` carries the
//! chain `*snow.Context`, the `*config.Config`, the parsed fxs, the
//! `TypeToFxIndex` routing map, the codec `Manager`, the `FeeAssetID`, and a
//! `Bootstrapped` flag. The Rust port keeps the same shape but narrowed to what
//! the **stateless** syntactic verifier (M5.12) actually reads:
//!
//! * `network_id` / `blockchain_id` — for the embedded `avax.BaseTx.Verify`
//!   (`vms/components/avax.BaseTx.Verify` network/chain id + memo checks).
//! * `tx_fee` / `create_asset_tx_fee` / `fee_asset_id` — for the per-tx
//!   `avax.VerifyTx` flow check (the burned-fee `Produce`).
//! * `num_fxs` — for `InitialState.Verify(codec, numFxs)` (specs 09 §3.3).
//! * `bootstrapped` — preserved for parity (the syntactic pass does not gate on
//!   it; the semantic verifier (M5.13) will).
//!
//! The codec [`Manager`] and the `TypeToFxIndex` routing table are process-wide
//! singletons ([`crate::txs::codec::Codec`] / [`crate::fx::dispatch`]), so —
//! unlike Go — they are not stored as fields here. The full VM `Config` lands in
//! M5.19.

use ava_types::id::Id;

/// The subset of `vms/avm/config.Config` the syntactic verifier reads
/// (specs 09 §6). The full config (upgrade times, mempool limits, …) is M5.19.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// `Config.TxFee` — the AVAX fee burned by a `BaseTx`/`OperationTx`/
    /// `ImportTx`/`ExportTx`.
    pub tx_fee: u64,
    /// `Config.CreateAssetTxFee` — the (usually higher) AVAX fee burned by a
    /// `CreateAssetTx`.
    pub create_asset_tx_fee: u64,
}

impl Config {
    /// Builds a [`Config`] from the two fee fields.
    #[must_use]
    pub fn new(tx_fee: u64, create_asset_tx_fee: u64) -> Self {
        Self {
            tx_fee,
            create_asset_tx_fee,
        }
    }
}

/// `executor.Backend` — the verification context shared by the syntactic and
/// semantic verifiers (specs 09 §6). The codec/routing tables are process-wide
/// singletons, so only the chain-context + fee + fx-count fields live here.
#[derive(Clone, Copy, Debug)]
pub struct Backend {
    /// `Ctx.NetworkID` — the network this chain lives on.
    pub network_id: u32,
    /// `Ctx.ChainID` — this chain's id (prevents cross-chain replay).
    pub blockchain_id: Id,
    /// The fee schedule (`Config.TxFee` / `Config.CreateAssetTxFee`).
    pub config: Config,
    /// `Backend.FeeAssetID` — the asset fees are paid in (may differ from
    /// `Ctx.AVAXAssetID` when this AVM runs in a subnet).
    pub fee_asset_id: Id,
    /// `len(Backend.Fxs)` — the number of registered feature extensions, the
    /// `numFxs` bound passed to `InitialState.Verify` (specs 09 §3.3).
    pub num_fxs: usize,
    /// `Backend.Bootstrapped` — whether the VM has finished bootstrapping.
    pub bootstrapped: bool,
}

impl Backend {
    /// Builds a [`Backend`] for the syntactic verifier.
    #[must_use]
    pub fn new(
        network_id: u32,
        blockchain_id: Id,
        config: Config,
        fee_asset_id: Id,
        num_fxs: usize,
        bootstrapped: bool,
    ) -> Self {
        Self {
            network_id,
            blockchain_id,
            config,
            fee_asset_id,
            num_fxs,
            bootstrapped,
        }
    }
}
