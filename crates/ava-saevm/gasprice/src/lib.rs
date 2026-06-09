// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-gasprice` — the SAE gas-price and fee-history estimator over the
//! executed/settled frontier (specs/11 §3).
//!
//! Provides `eth_gasPrice` ([`Estimator::suggest_gas_tip_cap`]) and
//! `eth_feeHistory` ([`Estimator::fee_history`]) by analyzing recently accepted
//! blocks. Port of Go `vms/saevm/gasprice`.
//!
//! Decoupled from M7.11 blocks via the [`Backend`] trait: callers adapt their
//! own block type into the lightweight [`Block`] projection.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

mod block_cache;
mod config;

use std::sync::Mutex;

use ava_saevm_intmath::bounded_sub;
use ava_saevm_types::U256;

pub use crate::block_cache::{Backend, BackendError, Block, BlockNumberRef, Tx};
pub use crate::config::{Clock, Config, ConfigError, ETHER, GWEI, WEI, system_clock};

/// The maximum number of percentiles accepted by
/// [`Estimator::fee_history`].
pub const MAX_PERCENTILES: usize = 100;

/// Errors returned by the estimator's query methods.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EstimatorError {
    /// A reward percentile was out of `[0, 100]` or not strictly ascending, or
    /// too many percentiles were requested.
    #[error("percentile out of range or misordered")]
    BadPercentile,
    /// The requested block is too far behind the accepted head.
    #[error("requested block is too far behind accepted head")]
    HistoryDepthExhausted,
    /// A block in the requested range was missing from the backend.
    #[error("missing block: {0}")]
    MissingBlock(u64),
    /// The backend failed to resolve or fetch a block.
    #[error(transparent)]
    Backend(#[from] BackendError),
}

/// The last-suggested tip cache (port of Go `last`).
struct Last {
    number: u64,
    price: U256,
}

/// Gas-price suggestions and fee-history data for SAE.
///
/// Port of Go `gasprice.Estimator`. Unlike Go, this port does **not** spawn a
/// background block-caching goroutine: caching is the [`Backend`]'s concern, so
/// the estimator stays a pure read-over-`Backend` projection. The `last`-tip
/// memoization is retained.
pub struct Estimator<B> {
    backend: B,
    c: Config,
    last: Mutex<Last>,
}

impl<B: Backend> Estimator<B> {
    /// Creates an [`Estimator`].
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] if `c` is invalid (port of Go validation in
    /// `NewEstimator`).
    pub fn new(backend: B, c: Config) -> Result<Self, ConfigError> {
        c.validate()?;
        let price = c.min_suggested_tip;
        Ok(Self {
            backend,
            c,
            last: Mutex::new(Last { number: 0, price }),
        })
    }

    /// Recommends a priority-fee (tip) for new transactions based on tips from
    /// recently accepted transactions.
    ///
    /// Port of Go `SuggestGasTipCap`. Unlike Go this is infallible: the chain
    /// walk never errors (missing blocks simply truncate it), and lock poison
    /// is recovered in place.
    #[must_use]
    pub fn suggest_gas_tip_cap(&self) -> U256 {
        let last_accepted_number = self.backend.last_accepted_number();

        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last_accepted_number <= last.number {
            return last.price;
        }

        let newest = last_accepted_number;
        let too_old = bounded_sub(newest, self.c.suggested_tip_max_blocks, 0);
        let recent_unix =
            (self.c.now)().saturating_sub(self.c.suggested_tip_max_duration.as_secs());

        let mut tips: Vec<Tx> = Vec::new();
        let mut n = newest;
        while n > too_old {
            match self.backend.block_by_number(n) {
                Some(b) if b.timestamp >= recent_unix => {
                    tips.extend(b.txs);
                }
                _ => break,
            }
            n = match n.checked_sub(1) {
                Some(next) => next,
                None => break,
            };
        }

        let mut price = last.price;
        if !tips.is_empty() {
            tips.sort_by_key(|t| t.tip);
            // i = (n-1) * percentile / 100; percentile is in (0,100].
            let len_minus_one = tips.len().saturating_sub(1);
            let pct = usize::try_from(self.c.suggested_tip_percentile).unwrap_or(100);
            let i = len_minus_one
                .saturating_mul(pct)
                .checked_div(100)
                .unwrap_or(0);
            price = tips[i].tip;
            price = price.max(self.c.min_suggested_tip);
            price = price.min(self.c.max_suggested_tip);
        }

        last.number = last_accepted_number;
        last.price = price;
        price
    }

    /// Returns data relevant for fee estimation over the specified range of
    /// blocks.
    ///
    /// Port of Go `FeeHistory`. Returns:
    /// - the first (lowest) block height of the processed range,
    /// - per-block reward percentiles (gas-weighted tips),
    /// - per-block base fees plus the next block's upper-bound base fee,
    /// - the fill ratio of each block.
    ///
    /// # Errors
    ///
    /// Returns [`EstimatorError`] on bad percentiles, exhausted history depth,
    /// missing blocks, or backend failures.
    #[allow(clippy::type_complexity)]
    pub fn fee_history(
        &self,
        blocks: u64,
        last_block: BlockNumberRef,
        reward_percentiles: &[f64],
    ) -> Result<(u64, Vec<Vec<U256>>, Vec<U256>, Vec<f64>), EstimatorError> {
        validate_percentiles(reward_percentiles)?;

        let last = self.backend.resolve_block_number(last_block)?;
        let last_accepted_number = self.backend.last_accepted_number();

        let min_last = bounded_sub(last_accepted_number, self.c.history_max_blocks_from_head, 0);
        if last < min_last {
            return Err(EstimatorError::HistoryDepthExhausted);
        }

        // min(requested, DoS bound, last+1 underflow protection).
        let blocks = blocks
            .min(self.c.history_max_blocks)
            .min(last.saturating_add(1));
        if blocks == 0 {
            return Ok((0, Vec::new(), Vec::new(), Vec::new()));
        }

        // first = last + 1 - blocks (blocks <= last+1, so this never underflows).
        let first = last.saturating_add(1).saturating_sub(blocks);

        let want_rewards = !reward_percentiles.is_empty();
        let mut rewards: Vec<Vec<U256>> = if want_rewards {
            Vec::with_capacity(usize::try_from(blocks).unwrap_or(0))
        } else {
            Vec::new()
        };
        let cap = usize::try_from(blocks).unwrap_or(0);
        let mut base_fees: Vec<U256> = Vec::with_capacity(cap.saturating_add(1));
        let mut gas_used_ratio: Vec<f64> = Vec::with_capacity(cap);

        let mut n = first;
        while n <= last {
            let b = self
                .backend
                .block_by_number(n)
                .ok_or(EstimatorError::MissingBlock(n))?;
            if want_rewards {
                rewards.push(b.tip_percentiles(reward_percentiles));
            }
            base_fees.push(b.base_fee);
            gas_used_ratio.push(gas_used_ratio_of(&b));
            n = n.checked_add(1).ok_or(EstimatorError::MissingBlock(n))?;
        }

        // The +1 base fee: the next block's upper-bound base fee.
        if last == last_accepted_number {
            // Worst-case bounds (M7.13) aren't available yet; the backend
            // returns them if it can, else we fall back to the last block's
            // header base fee. TODO(M7.13): wire LatestEndTime.BaseFee().
            let next_bf = match self.backend.next_block_upper_bound_base_fee() {
                Some(bf) => bf,
                None => {
                    self.backend
                        .block_by_number(last)
                        .ok_or(EstimatorError::MissingBlock(last))?
                        .base_fee
                }
            };
            base_fees.push(next_bf);
        } else {
            let next = last
                .checked_add(1)
                .ok_or(EstimatorError::MissingBlock(last))?;
            let b = self
                .backend
                .block_by_number(next)
                .ok_or(EstimatorError::MissingBlock(next))?;
            base_fees.push(b.base_fee);
        }

        Ok((first, rewards, base_fees, gas_used_ratio))
    }
}

/// Computes a block's fill ratio (`gas_used / gas_limit`).
// floats: RPC display value, non-consensus; precision loss acceptable.
#[allow(clippy::cast_precision_loss)]
#[allow(clippy::arithmetic_side_effects)]
fn gas_used_ratio_of(b: &Block) -> f64 {
    if b.gas_limit == 0 {
        return 0.0;
    }
    b.gas_used as f64 / b.gas_limit as f64
}

/// Validates reward percentiles: at most [`MAX_PERCENTILES`], each in
/// `[0, 100]`, strictly ascending. Port of Go `validatePercentiles`.
fn validate_percentiles(percentiles: &[f64]) -> Result<(), EstimatorError> {
    if percentiles.len() > MAX_PERCENTILES {
        return Err(EstimatorError::BadPercentile);
    }
    let mut prev: Option<f64> = None;
    for &p in percentiles {
        if !(0.0..=100.0).contains(&p) {
            return Err(EstimatorError::BadPercentile);
        }
        if let Some(prev) = prev
            && p <= prev
        {
            return Err(EstimatorError::BadPercentile);
        }
        prev = Some(p);
    }
    Ok(())
}
