// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ACP-103 dynamic gas fee (post-Etna) — `DynamicCalculator`.
//!
//! Port of Go `vms/platformvm/txs/fee/dynamic_calculator.go`. A tx's
//! complexity 4-vector is collapsed to scalar gas via the configured weights
//! (`gas::dot_to_gas`), then `fee = gas · price` where `price` is the
//! exponential `gas::calculate_price(min_price, state.excess, K)` (specs 21
//! §1).

use crate::error::{Error, Result};
use crate::txs::fee::gas::{Dimensions, calculate_price, dot_to_gas};

/// Per-dimension weights `[Bandwidth, DBRead, DBWrite, Compute]` (`gas.Config`).
///
/// Identical on mainnet and Fuji (specs 21 §1).
pub const WEIGHTS: Dimensions = [1, 1_000, 1_000, 4];

/// Maximum gas capacity per block (`gas.Config.MaxCapacity`).
pub const MAX_CAPACITY: u64 = 1_000_000;

/// Maximum gas consumable per second / capacity refill rate
/// (`gas.Config.MaxPerSecond`).
pub const MAX_PER_SECOND: u64 = 100_000;

/// Target gas consumption per second (`gas.Config.TargetPerSecond`), half of
/// [`MAX_PER_SECOND`].
pub const TARGET_PER_SECOND: u64 = 50_000;

/// Minimum gas price in nAVAX/gas (`gas.Config.MinPrice`).
pub const MIN_PRICE: u64 = 1;

/// The excess conversion constant `K` (`gas.Config.ExcessConversionConstant`).
///
/// `≈ (MaxPerSecond − TargetPerSecond) · 30 / ln2` ("double every 30 s");
/// hard-coded as an integer literal (specs 21 §1, identical mainnet & Fuji).
pub const K: u64 = 2_164_043;

/// The post-Etna dynamic fee calculator: `fee = (complexity · weights) · price`
/// (`fee.dynamicCalculator`).
#[derive(Clone, Copy, Debug)]
pub struct DynamicCalculator {
    weights: Dimensions,
    price: u64,
}

impl DynamicCalculator {
    /// Builds a calculator with the given per-dimension weights and gas price.
    #[must_use]
    pub fn new(weights: Dimensions, price: u64) -> Self {
        Self { weights, price }
    }

    /// Builds a calculator with the network [`WEIGHTS`] and a `price` derived
    /// from the current gas `excess` via [`calculate_price`].
    #[must_use]
    pub fn from_excess(excess: u64) -> Self {
        Self {
            weights: WEIGHTS,
            price: calculate_price(MIN_PRICE, excess, K),
        }
    }

    /// Returns the fee for a tx of the given `complexity`:
    /// `(complexity · weights) · price` (`dynamicCalculator.CalculateFee`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::FeeOverflow`] if the gas dot product or the
    /// `gas · price` cost overflows `u64`.
    pub fn calculate_fee(&self, complexity: Dimensions) -> Result<u64> {
        let gas = dot_to_gas(complexity, self.weights)?;
        gas.checked_mul(self.price).ok_or(Error::FeeOverflow)
    }
}
