// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Static (pre-Etna) tx fees — `SimpleCalculator`.
//!
//! Port of Go `vms/platformvm/txs/fee/simple_calculator.go`. `CalculateFee`
//! returns a flat `tx_fee` independent of the tx; the per-network constants
//! come from genesis (`genesis/params.go::TxFeeConfig`, specs 21 §2a).

/// One milliAVAX in nAVAX (`units.MilliAvax`); the base unit for static fees.
pub const MILLI_AVAX: u64 = 1_000_000;

/// Static, per-network tx fee constants (`genesis.TxFeeConfig`, specs 21 §2a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticFeeConfig {
    /// Flat fee charged for a standard tx (`TxFee`).
    pub tx_fee: u64,
    /// Flat fee charged for an asset-creation tx (`CreateAssetTxFee`).
    pub create_asset_tx_fee: u64,
}

impl StaticFeeConfig {
    /// Mainnet static fees: `TxFee = MilliAvax`, `CreateAssetTxFee =
    /// 10·MilliAvax` (specs 21 §2a).
    pub const MAINNET: StaticFeeConfig = StaticFeeConfig {
        tx_fee: MILLI_AVAX,
        create_asset_tx_fee: 10 * MILLI_AVAX,
    };

    /// Fuji static fees, identical to mainnet (specs 21 §2a).
    pub const FUJI: StaticFeeConfig = StaticFeeConfig {
        tx_fee: MILLI_AVAX,
        create_asset_tx_fee: 10 * MILLI_AVAX,
    };

    /// Local-network static fees: `TxFee = CreateAssetTxFee = MilliAvax`
    /// (specs 21 §2a).
    pub const LOCAL: StaticFeeConfig = StaticFeeConfig {
        tx_fee: MILLI_AVAX,
        create_asset_tx_fee: MILLI_AVAX,
    };
}

/// The pre-Etna static fee calculator: returns a stored flat fee regardless of
/// the tx (`fee.SimpleCalculator`).
#[derive(Clone, Copy, Debug)]
pub struct SimpleCalculator {
    tx_fee: u64,
}

impl SimpleCalculator {
    /// Builds a calculator returning the given flat `tx_fee`.
    #[must_use]
    pub fn new(tx_fee: u64) -> Self {
        Self { tx_fee }
    }

    /// Returns the flat tx fee, independent of any transaction
    /// (`SimpleCalculator.CalculateFee`).
    #[must_use]
    pub fn calculate_fee(&self) -> u64 {
        self.tx_fee
    }
}

#[cfg(test)]
mod golden {
    use super::*;

    #[test]
    fn static_fee_constants() {
        assert_eq!(StaticFeeConfig::MAINNET.tx_fee, 1_000_000);
        assert_eq!(StaticFeeConfig::MAINNET.create_asset_tx_fee, 10_000_000);
        assert_eq!(StaticFeeConfig::FUJI, StaticFeeConfig::MAINNET);
        assert_eq!(StaticFeeConfig::LOCAL.create_asset_tx_fee, 1_000_000);
        assert_eq!(
            SimpleCalculator::new(StaticFeeConfig::MAINNET.tx_fee).calculate_fee(),
            1_000_000
        );
    }
}
