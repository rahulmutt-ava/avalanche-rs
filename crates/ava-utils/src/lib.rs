// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-utils` — standalone shared utilities.
//!
//! Tier T0 (primitives), standalone. Owning specs:
//! `specs/03-core-primitives.md` §4 and `specs/24-determinism-and-clock.md`.
//! Implemented across M0:
//!
//! - [`rng`] — gonum-exact MT19937 / MT19937-64 (the R1 gate, M0.3)
//! - [`sampler`] — `Uint64Inclusive` + uniform/weighted/wwr samplers (M0.4, M0.10)
//! - [`set`] / [`bag`] / [`bits`] / [`linked`] — collections (M0.9)
//! - [`math`] / [`units`] — checked arithmetic + unit constants (M0.9)
//! - [`cb58`] — CB58 codec shared by ava-types & ava-crypto (M0.11)
//! - [`clock`] — injectable Real/Mock clock (M0.12)
//! - [`error`] — the crate error enum
//!
//! Modules are scaffolded empty in M0.1 and filled in by their owning tasks.

#![forbid(unsafe_code)]

pub mod bag;
pub mod bits;
pub mod cb58;
pub mod clock;
pub mod error;
pub mod linked;
pub mod math;
pub mod rng;
pub mod sampler;
pub mod set;
pub mod units;
