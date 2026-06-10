// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cchain::dynamic` — ACP-176 / ACP-226 / ACP-283 exponential-integrator
//! exponent types for the C-Chain SAE consensus parameters.
//!
//! Each parameter is encoded as a `u64` exponent newtype. All three reuse
//! [`ava_vm::components::gas::calculate_price`] as the reader:
//!
//! ```text
//! value = minimum · e^(exponent / K)
//! ```
//!
//! Two shared generic helpers, `toward` and `search`, drive the per-block
//! ramp (clamped step) and the inversion (binary-search `Desired*` functions).
//!
//! Port of Go `vms/saevm/cchain/dynamic` (`2750cc9e42`, #5481).

mod math;

pub mod delay;
pub mod price;
pub mod target;

pub use delay::{DelayExponent, desired_delay_exponent};
pub use price::{PriceExponent, desired_price_exponent};
pub use target::{TargetExponent, desired_target_exponent};
