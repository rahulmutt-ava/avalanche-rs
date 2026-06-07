// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-reward calculator — port of `vms/platformvm/reward`.
//!
//! Primary-Network minting from genesis: the reward a staker earns is a pure
//! `big.Int` (→ [`num_bigint::BigUint`]) function of `(stakedDuration, stake,
//! currentSupply)` against a frozen [`Config`]. There is no exponential — this
//! is its own integer formula (cf. the gas/fee exponential in `gastime`).
//!
//! The math is byte/integer-exact with Go: every multiplication precedes any
//! division and the three trailing divides are separate truncating steps. See
//! `specs/08-platformvm-pchain.md` §5 and `specs/21-fee-economics-math.md` §3.

mod calculator;
mod config;

pub use calculator::{Calculator, split};
pub use config::{Config, PERCENT_DENOMINATOR};
