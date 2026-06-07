// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain transaction fees (`vms/platformvm/txs/fee`).
//!
//! Two fee regimes, selected by fork (specs 08 §6, 21 §1/§2a):
//!
//! - **Static (pre-Etna)** — [`simple_calculator::SimpleCalculator`] returns a
//!   flat per-network fee regardless of the tx.
//! - **Dynamic (post-Etna / ACP-103)** — [`dynamic_calculator::DynamicCalculator`]
//!   charges `fee = (complexity · weights) · price`, where `price` is the
//!   exponential of the on-chain gas excess.
//!
//! Both build on the shared [`gas`] primitives (the `calculate_price`
//! exponential, the `GasState` meter, and the `dot_to_gas` dot product).

pub mod complexity;
pub mod dynamic_calculator;
pub mod gas;
pub mod simple_calculator;

use crate::error::Result;
use dynamic_calculator::DynamicCalculator;
use gas::Dimensions;
use simple_calculator::SimpleCalculator;

/// The fee regime in force for a block, selected by fork (specs 08 §6).
///
/// Pre-Etna uses the flat static fee; post-Etna uses the ACP-103 dynamic gas
/// fee derived from the on-chain gas excess.
#[derive(Clone, Copy, Debug)]
pub enum FeeCalculator {
    /// The pre-Etna static (flat) fee calculator.
    Static(SimpleCalculator),
    /// The post-Etna ACP-103 dynamic gas fee calculator.
    Dynamic(DynamicCalculator),
}

impl FeeCalculator {
    /// Selects the fee regime by fork: [`FeeCalculator::Dynamic`] when Etna is
    /// active, else [`FeeCalculator::Static`] (specs 08 §6).
    ///
    /// `tx_fee` is the static per-network flat fee; `excess` is the current
    /// on-chain gas excess feeding the dynamic price.
    #[must_use]
    pub fn for_fork(etna_active: bool, tx_fee: u64, excess: u64) -> Self {
        if etna_active {
            FeeCalculator::Dynamic(DynamicCalculator::from_excess(excess))
        } else {
            FeeCalculator::Static(SimpleCalculator::new(tx_fee))
        }
    }

    /// Returns the fee for a tx of the given `complexity`.
    ///
    /// The static regime ignores `complexity` and returns the flat fee; the
    /// dynamic regime computes `(complexity · weights) · price`.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::error::Error::FeeOverflow`] from the dynamic path.
    pub fn calculate_fee(&self, complexity: Dimensions) -> Result<u64> {
        match self {
            FeeCalculator::Static(c) => Ok(c.calculate_fee()),
            FeeCalculator::Dynamic(c) => c.calculate_fee(complexity),
        }
    }
}
