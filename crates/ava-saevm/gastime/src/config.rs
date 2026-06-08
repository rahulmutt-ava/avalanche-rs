// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Gas-pricing configuration for the SAE gas clock.
//!
//! Port of `vms/saevm/gastime/config.go`'s `GasPriceConfig`.

/// The default ratio between the gas target and the reciprocal of the excess
/// coefficient used in price calculation (the `K` variable in ACP-176, where
/// `K = target_to_excess_scaling * T`).
///
/// Mirrors Go's `DefaultTargetToExcessScaling`.
pub const DEFAULT_TARGET_TO_EXCESS_SCALING: u64 = 87;

/// The default minimum gas price (base fee) — the `M` parameter in ACP-176's
/// price calculation.
///
/// Mirrors Go's `DefaultMinPrice`.
pub const DEFAULT_MIN_PRICE: u64 = 1;

/// Gas-related parameters that can be configured via hooks.
///
/// Port of `vms/saevm/gastime/config.go::GasPriceConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GasPriceConfig {
    /// The minimum gas price / base fee (the `M` parameter in ACP-176).
    min_price: u64,
    /// The ratio between the gas target and the reciprocal of the excess
    /// coefficient used in price calculation (the `K` variable in ACP-176,
    /// where `K = target_to_excess_scaling * T`).
    target_to_excess_scaling: u64,
    /// Whether the gas price should be static at the minimum price.
    static_pricing: bool,
}

impl GasPriceConfig {
    /// Constructs a [`GasPriceConfig`] from its component fields.
    #[must_use]
    pub fn new(min_price: u64, target_to_excess_scaling: u64, static_pricing: bool) -> Self {
        Self {
            min_price,
            target_to_excess_scaling,
            static_pricing,
        }
    }

    /// Returns the minimum gas price / base fee (the `M` parameter).
    #[must_use]
    pub fn min_price(&self) -> u64 {
        self.min_price
    }

    /// Returns the `target_to_excess_scaling` ratio (used to derive `K`).
    #[must_use]
    pub fn target_to_excess_scaling(&self) -> u64 {
        self.target_to_excess_scaling
    }

    /// Returns whether static pricing is enabled.
    #[must_use]
    pub fn static_pricing(&self) -> bool {
        self.static_pricing
    }
}

impl Default for GasPriceConfig {
    /// Mirrors Go's `DefaultGasPriceConfig`: scaling 87, min price 1, dynamic
    /// pricing.
    fn default() -> Self {
        Self {
            min_price: DEFAULT_MIN_PRICE,
            target_to_excess_scaling: DEFAULT_TARGET_TO_EXCESS_SCALING,
            static_pricing: false,
        }
    }
}
