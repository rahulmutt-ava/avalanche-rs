// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The lightweight per-block projection the estimator reads, plus the
//! [`Backend`] trait that yields it. Port of Go `block_cache.go`.

use ava_saevm_types::U256;

/// A reference to a block, mirroring Go `rpc.BlockNumber`.
///
/// The estimator resolves this to a concrete height via
/// [`Backend::resolve_block_number`] before fetching blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockNumberRef {
    /// The genesis (earliest) block, height `0`.
    Earliest,
    /// The latest accepted block.
    Latest,
    /// The pending block. SAE has no pending block distinct from latest, so
    /// this resolves the same as [`BlockNumberRef::Latest`].
    Pending,
    /// An absolute block height.
    Number(u64),
}

/// A transaction's contribution to the tip distribution.
///
/// Port of the Go `transaction` struct: `gas` is the tx gas *limit* (not gas
/// charged — SAE sequences without executing), `tip` is the effective gas tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tx {
    /// The transaction's gas limit.
    pub gas: u64,
    /// The transaction's effective gas tip.
    pub tip: U256,
}

/// The estimator's lightweight projection of a chain block.
///
/// Port of the Go `block` struct. `txs` MUST be sorted ascending by tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Header timestamp (Unix seconds).
    pub timestamp: u64,
    /// Header gas used.
    pub gas_used: u64,
    /// Header gas limit.
    pub gas_limit: u64,
    /// Header base fee.
    pub base_fee: U256,
    /// Transactions, sorted ascending by tip.
    pub txs: Vec<Tx>,
}

impl Block {
    /// Ensures `txs` are sorted ascending by tip (defensive; callers and
    /// [`Backend`] implementations are expected to provide them sorted).
    pub fn sort_txs(&mut self) {
        self.txs.sort_by_key(|t| t.tip);
    }

    /// Computes the gas-weighted tip at each requested percentile.
    ///
    /// `percentiles` MUST be sorted ascending. Port of Go `tipPercentiles`.
    ///
    /// Because block builders sequence transactions without executing them in
    /// SAE, gas *limits* are accumulated, not gas charged.
    ///
    /// The threshold is computed exactly as Go does — `uint64(float64(gasUsed) *
    /// p / 100)` — by truncating the `f64` product toward zero to a `u64` BEFORE
    /// comparing. The accumulator comparison `sum_gas < threshold` is then a pure
    /// integer compare. Doing the truncation first matters: when the float
    /// threshold has a fractional part landing exactly on an integer `sum_gas`
    /// (`floor(t) == sum_gas < t`), Go stops the loop; a float compare would
    /// advance one extra tx (an off-by-one vs Go).
    #[must_use]
    // floats: no overflow-panic; RPC estimator, non-consensus path. Widening
    // u64 gas to f64 is cast_precision_loss only (RPC display math).
    #[allow(clippy::arithmetic_side_effects)]
    #[allow(clippy::cast_precision_loss)]
    pub fn tip_percentiles(&self, percentiles: &[f64]) -> Vec<U256> {
        let mut out = Vec::with_capacity(percentiles.len());
        if self.txs.is_empty() {
            out.resize(percentiles.len(), U256::ZERO);
            return out;
        }

        let mut tx_index: usize = 0;
        let mut sum_gas: u64 = self.txs[0].gas;
        let gas_used = self.gas_used as f64;
        for &p in percentiles {
            let t = gas_used * p / 100.0;
            // Go-faithful truncating threshold; t in [0, gas_used] (p <= 100),
            // non-negative after trunc, fits u64. Mirrors Go's `uint64(...)`.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let threshold: u64 = t.trunc() as u64;
            while sum_gas < threshold && tx_index < self.txs.len().saturating_sub(1) {
                tx_index = tx_index.saturating_add(1);
                sum_gas = sum_gas.saturating_add(self.txs[tx_index].gas);
            }
            out.push(self.txs[tx_index].tip);
        }
        out
    }
}

/// Yields the chain data the [`Estimator`](crate::Estimator) depends on.
///
/// Port of the Go `Backend` interface, projected down to what the estimator
/// actually reads. Deliberately does **not** depend on `ava-saevm-blocks`
/// (M7.11); implementations adapt their own block type into [`Block`].
pub trait Backend {
    /// Resolves a [`BlockNumberRef`] to a concrete height.
    ///
    /// # Errors
    ///
    /// Returns an error if the reference cannot be resolved (e.g. a numbered
    /// block beyond the accepted head).
    fn resolve_block_number(&self, bn: BlockNumberRef) -> Result<u64, BackendError>;

    /// Returns the [`Block`] projection at height `n`, or `None` if absent.
    fn block_by_number(&self, n: u64) -> Option<Block>;

    /// Returns the height of the last-accepted block.
    fn last_accepted_number(&self) -> u64;

    /// Returns the next block's upper-bound base fee (the worst-case-bounds
    /// `+1` base fee used by [`fee_history`](crate::Estimator::fee_history)
    /// when `last == last_accepted`).
    ///
    /// Returns `None` if the worst-case bounds are unavailable, in which case
    /// the estimator falls back to the last block's header base fee.
    ///
    /// TODO(M7.13): once worst-case bounds (M7.13) land, back this with the
    /// last-accepted block's `WorstCaseBounds.LatestEndTime.BaseFee()`.
    fn next_block_upper_bound_base_fee(&self) -> Option<U256> {
        None
    }
}

/// Errors returned by [`Backend`] methods.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    /// The requested block does not exist (port of Go `ErrBlockNotFound`).
    #[error("block not found")]
    BlockNotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tip_percentiles_gas_weighted() {
        // Five 100k-gas txs, tips 1..=5; gas_used 500k.
        let txs = (1u64..=5)
            .map(|t| Tx {
                gas: 100_000,
                tip: U256::from(t),
            })
            .collect();
        let b = Block {
            timestamp: 0,
            gas_used: 500_000,
            gas_limit: 1_000_000,
            base_fee: U256::ZERO,
            txs,
        };
        assert_eq!(
            b.tip_percentiles(&[25.0, 50.0, 75.0]),
            vec![U256::from(2u64), U256::from(3u64), U256::from(4u64)]
        );
    }

    #[test]
    fn tip_percentiles_fractional_threshold_truncates_like_go() {
        // Two 50k-gas txs (tips 1, 2); gas_used 100_001 so the threshold has a
        // fractional part that lands exactly on the first cumulative-gas
        // boundary: 100_001 * 50 / 100 = 50_000.5, floor = 50_000 == sum_gas
        // after tx[0].
        //
        // Go truncates first: threshold = uint64(50_000.5) = 50_000, then
        // `50_000 < 50_000` is false, so the loop STOPS at tx[0] (tip 1).
        // A naive f64 compare `50_000.0 < 50_000.5` is true and would advance
        // one extra tx to tx[1] (tip 2) — the off-by-one this guards against.
        let txs = vec![
            Tx {
                gas: 50_000,
                tip: U256::from(1u64),
            },
            Tx {
                gas: 50_000,
                tip: U256::from(2u64),
            },
        ];
        let b = Block {
            timestamp: 0,
            gas_used: 100_001,
            gas_limit: 1_000_000,
            base_fee: U256::ZERO,
            txs,
        };
        // Must select tip 1 (Go's truncating behavior), not tip 2.
        assert_eq!(b.tip_percentiles(&[50.0]), vec![U256::from(1u64)]);
    }

    #[test]
    fn tip_percentiles_empty_block() {
        let b = Block {
            timestamp: 0,
            gas_used: 0,
            gas_limit: 1_000_000,
            base_fee: U256::ZERO,
            txs: Vec::new(),
        };
        assert_eq!(b.tip_percentiles(&[50.0]), vec![U256::ZERO]);
    }
}
