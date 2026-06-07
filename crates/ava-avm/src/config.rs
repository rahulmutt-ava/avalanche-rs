// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain (AVM) VM configuration (`vms/avm/config/config.go`, specs 09 §6).
//!
//! Go's `avm` reads its per-chain config from the JSON `configBytes` the engine
//! hands `Initialize`; the only fields the verifier consumes are the two fee
//! amounts (`TxFee` / `CreateAssetTxFee`). This [`Config`] is the minimal port:
//! a JSON-decodable struct with those two fields, defaulting to the
//! avalanchego-mainnet values when `config_bytes` is empty (the engine passes
//! `b""` when no chain config file is present).
//!
//! ## Deferred
//!
//! The full Go `config.Config` (network-upgrade times, index/checksum toggles,
//! mempool bounds, …) is the M8/`ava-genesis` follow-up — the mempool bounds are
//! compile-time constants in [`crate::mempool`] and the upgrade schedule is not
//! yet consumed by the X-Chain verifiers.

use crate::error::{Error, Result};

/// `DefaultTxFee` — the avalanchego-mainnet AVAX fee (nAVAX) burned by a
/// `BaseTx`/`OperationTx`/`ImportTx`/`ExportTx` (Go `genesis` mainnet params,
/// 1,000,000 nAVAX = 0.001 AVAX).
pub const DEFAULT_TX_FEE: u64 = 1_000_000;

/// `DefaultCreateAssetTxFee` — the avalanchego-mainnet AVAX fee (nAVAX) burned
/// by a `CreateAssetTx` (Go `genesis` mainnet params, 10,000,000 nAVAX =
/// 0.01 AVAX).
pub const DEFAULT_CREATE_ASSET_TX_FEE: u64 = 10_000_000;

/// `config.Config` — the X-Chain VM configuration (specs 09 §6).
///
/// Mirrors the fee subset of Go `vms/avm/config.Config`. Decoded from the JSON
/// `config_bytes` the engine supplies at `initialize`, falling back to
/// [`Config::default`] (the mainnet fees) when those bytes are empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// `Config.TxFee` — the AVAX fee burned by a non-asset-creation tx.
    pub tx_fee: u64,
    /// `Config.CreateAssetTxFee` — the (higher) AVAX fee burned by a
    /// `CreateAssetTx`.
    pub create_asset_tx_fee: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tx_fee: DEFAULT_TX_FEE,
            create_asset_tx_fee: DEFAULT_CREATE_ASSET_TX_FEE,
        }
    }
}

impl Config {
    /// Parses the VM config from the engine-supplied `config_bytes`.
    ///
    /// Empty bytes (the engine's "no chain config file" sentinel) yield
    /// [`Config::default`]; otherwise the bytes are decoded as JSON. Unknown
    /// JSON fields are accepted (forward-compatible with the not-yet-ported
    /// config fields) and missing fields fall back to the defaults (via
    /// `#[serde(default)]`).
    ///
    /// # Errors
    /// Returns [`Error::Config`] if non-empty `config_bytes` is not valid JSON
    /// for this struct.
    pub fn parse(config_bytes: &[u8]) -> Result<Self> {
        if config_bytes.is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_slice(config_bytes).map_err(|e| Error::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bytes_are_mainnet_defaults() {
        let c = Config::parse(b"").expect("parse empty");
        assert_eq!(c.tx_fee, DEFAULT_TX_FEE);
        assert_eq!(c.create_asset_tx_fee, DEFAULT_CREATE_ASSET_TX_FEE);
    }

    #[test]
    fn json_overrides_fees() {
        let c = Config::parse(br#"{"txFee":7,"createAssetTxFee":9}"#).expect("parse json");
        assert_eq!(c.tx_fee, 7);
        assert_eq!(c.create_asset_tx_fee, 9);
    }

    #[test]
    fn partial_json_keeps_defaults() {
        let c = Config::parse(br#"{"txFee":3}"#).expect("parse partial");
        assert_eq!(c.tx_fee, 3);
        assert_eq!(c.create_asset_tx_fee, DEFAULT_CREATE_ASSET_TX_FEE);
    }

    #[test]
    fn malformed_json_errors() {
        assert!(Config::parse(b"not json").is_err());
    }
}
