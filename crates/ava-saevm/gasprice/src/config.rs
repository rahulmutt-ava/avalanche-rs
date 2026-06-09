// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Estimator configuration and validation (port of Go `gasprice.Config`).

use core::time::Duration;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ava_saevm_types::U256;

/// Wei is the smallest denomination of the native asset (`aAVAX`).
pub const WEI: u64 = 1;
/// `GWei` is `1e9` Wei (`nAVAX`).
pub const GWEI: u64 = 1_000_000_000;
/// Ether is `1e18` Wei (`AVAX`).
pub const ETHER: u128 = 1_000_000_000_000_000_000;

/// A clock returning the current Unix time in seconds.
///
/// Mirrors Go's `Now func() time.Time`; injected for testability. The estimator
/// only ever uses the value as a Unix-second cutoff, so the closure yields
/// seconds directly.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Returns a [`Clock`] reading the system wall clock (Unix seconds).
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

/// Parameterizes an [`Estimator`](crate::Estimator).
///
/// Port of the Go `gasprice.Config` struct.
#[derive(Clone)]
pub struct Config {
    /// Returns the current time as Unix seconds.
    pub now: Clock,

    /// Minimum suggested tip, and the default tip when no better estimate can be
    /// made.
    pub min_suggested_tip: U256,
    /// In the range `(0, 100]`: which percentile of recent tips is used when
    /// suggesting a tip from recent transactions.
    pub suggested_tip_percentile: u64,
    /// Maximum suggested tip.
    pub max_suggested_tip: U256,

    /// Maximum number of recent blocks to fetch for
    /// [`suggest_gas_tip_cap`](crate::Estimator::suggest_gas_tip_cap).
    pub suggested_tip_max_blocks: u64,
    /// How long a block is considered recent for
    /// [`suggest_gas_tip_cap`](crate::Estimator::suggest_gas_tip_cap).
    pub suggested_tip_max_duration: Duration,

    /// The furthest `last_block` behind the last-accepted block that
    /// [`fee_history`](crate::Estimator::fee_history) will serve.
    pub history_max_blocks_from_head: u64,
    /// Maximum number of blocks fetched in a single
    /// [`fee_history`](crate::Estimator::fee_history) call.
    pub history_max_blocks: u64,
}

impl Config {
    /// Returns a [`Config`] with all fields set to their default values.
    ///
    /// Port of Go `DefaultConfig()`.
    #[must_use]
    pub fn default_config() -> Self {
        Self {
            now: system_clock(),
            min_suggested_tip: U256::from(WEI),
            // Chosen below the median of recent tips to avoid a self-induced fee
            // spiral.
            suggested_tip_percentile: 40,
            max_suggested_tip: U256::from(150u64.saturating_mul(WEI)),
            suggested_tip_max_blocks: 20,
            suggested_tip_max_duration: Duration::from_mins(1),
            // Larger than MetaMask's 20k-block fee lookback window.
            history_max_blocks_from_head: 25_000,
            history_max_blocks: 2048,
        }
    }

    /// Returns an error if the config is invalid (port of Go `validate`).
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] describing the first invalid field.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.suggested_tip_percentile == 0 || self.suggested_tip_percentile > 100 {
            return Err(ConfigError::BadTipPercentile);
        }
        if self.min_suggested_tip > self.max_suggested_tip {
            return Err(ConfigError::MinTipExceedsMax);
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Errors returned by [`Config::validate`].
///
/// The Rust port folds Go's nil-pointer checks (`errNilNow`,
/// `errNilMinSuggestedTip`, `errNilMaxSuggestedTip`) away because the
/// corresponding fields are non-optional value types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `suggested_tip_percentile` must be in `(0, 100]`.
    #[error("config suggested_tip_percentile must be in (0, 100]")]
    BadTipPercentile,
    /// `min_suggested_tip` must be `<= max_suggested_tip`.
    #[error("config min_suggested_tip must be <= max_suggested_tip")]
    MinTipExceedsMax,
}
