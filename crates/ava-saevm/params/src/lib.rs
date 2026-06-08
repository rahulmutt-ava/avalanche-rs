// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-params` — SAE (ACP-194) protocol parameters: the Tau-discipline
//! `BlockInstant` (no `Add<u64>`), `Lambda`, and the derived block/queue limits
//! (specs/11 §2.3/§2.4). M7.1 lands only the scaffold + `TAU_SECONDS`; the full
//! Tau discipline is M7.2.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

/// Tau, the SAE settlement delay, in seconds (specs/11 §2.3). The full
/// `params::TAU: Duration` and the Tau-discipline `BlockInstant` land in M7.2.
pub const TAU_SECONDS: u64 = 5;
