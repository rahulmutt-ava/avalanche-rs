// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-params` — SAE (ACP-194) protocol parameters: the Tau-discipline
//! [`BlockInstant`] (no `Add<u64>`), [`LAMBDA`], and the derived block/queue
//! limits (specs/11 §2.3/§2.4).
//!
//! Semantics mirror the Go reference `vms/saevm/params/params.go`. The
//! [`BlockInstant`] "Tau discipline" mirrors `vms/saevm/sae/block_builder.go`
//! `lastToSettle`, which computes the settlement instant as
//! `bTime.Add(-saeparams.Tau)` — i.e. by subtracting a `Duration` (Tau), never
//! by adding/subtracting a raw integer. The structural analog of the Go
//! `tausecondslint` check is enforced here by deliberately providing no
//! `impl Add<u64>`/`Sub<u64>` for [`BlockInstant`]: the only way to move a
//! `BlockInstant` is via [`BlockInstant::minus`]/[`BlockInstant::plus`], which
//! take a [`Duration`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `Lambda` is the denominator for computing the minimum gas consumed per
/// transaction. For a transaction with gas limit `g`, the minimum consumption
/// is `ceil(g/Lambda)`. Mirrors `params.go::Lambda`.
pub const LAMBDA: u64 = 2;

/// Tau, the SAE settlement delay, in seconds (`params.go::TauSeconds`).
pub const TAU_SECONDS: u64 = 5;

/// Tau is the minimum duration between a block's execution completing and the
/// resulting state changes being settled in a later block. Mirrors
/// `params.go::Tau` (`TauSeconds * time.Second`).
pub const TAU: Duration = Duration::from_secs(TAU_SECONDS);

/// The maximum number of full blocks that can be in the execution queue while
/// it remains open to accepting a new block. Mirrors
/// `params.go::MaxFullBlocksInOpenQueue`.
pub const MAX_FULL_BLOCKS_IN_OPEN_QUEUE: u64 = 2;

/// The maximum number of full blocks that can be in the execution queue.
/// Mirrors `params.go::MaxFullBlocksInClosedQueue`
/// (`MaxFullBlocksInOpenQueue + 1`).
pub const MAX_FULL_BLOCKS_IN_CLOSED_QUEUE: u64 = MAX_FULL_BLOCKS_IN_OPEN_QUEUE + 1;

/// The multiplier applied to [`TAU`] to derive [`MAX_QUEUE_WALL_TIME`]:
/// `MaxFullBlocksInClosedQueue * Lambda = 3 * 2 = 6`. Held as a `u32` because
/// [`Duration::saturating_mul`] takes a `u32`. Derived as a const literal (the
/// inputs are compile-time constants) to avoid a non-const `u32::try_from` in
/// const context; the value is re-checked against the inputs in
/// `tests/tau_discipline.rs`.
const MAX_QUEUE_WALL_TIME_MULTIPLIER: u32 = 6;

/// The maximum wall-clock duration a block should remain in the execution queue
/// before execution finishes. Mirrors `params.go::MaxQueueWallTime`
/// (`MaxFullBlocksInClosedQueue * Tau * Lambda = 3 * 5s * 2 = 30s`).
pub const MAX_QUEUE_WALL_TIME: Duration = TAU.saturating_mul(MAX_QUEUE_WALL_TIME_MULTIPLIER);

/// The furthest into the future (relative to the builder's local clock) that a
/// block's timestamp may be. Mirrors the `maxFutureBlockDuration` used in
/// `vms/saevm/sae/block_builder.go::lastToSettle` (lives in the block builder,
/// not `params.go`).
pub const MAX_FUTURE_BLOCK: Duration = Duration::from_secs(10);

/// A block timestamp, as a point in wall-clock time at or after the UNIX epoch.
///
/// The Tau discipline (see crate docs) is enforced structurally: there is no
/// `impl Add<u64>`/`Sub<u64>`. The only way to move a `BlockInstant` is via
/// [`BlockInstant::minus`]/[`BlockInstant::plus`], which take a [`Duration`]
/// and saturate at the UNIX epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockInstant(SystemTime);

impl BlockInstant {
    /// Constructs a `BlockInstant` `secs` seconds after the UNIX epoch.
    #[must_use]
    pub fn from_unix(secs: u64) -> Self {
        Self::from_unix_epoch().plus(Duration::from_secs(secs))
    }

    /// The UNIX epoch as a `BlockInstant` (the floor of the type).
    #[must_use]
    fn from_unix_epoch() -> Self {
        Self(UNIX_EPOCH)
    }

    /// Returns this instant moved earlier by `d`, saturating at the UNIX epoch
    /// (never goes below the epoch). Mirrors `bTime.Add(-saeparams.Tau)` in
    /// `block_builder.go::lastToSettle`.
    #[must_use]
    pub fn minus(self, d: Duration) -> Self {
        match self.0.checked_sub(d) {
            Some(t) if t >= UNIX_EPOCH => Self(t),
            _ => Self(UNIX_EPOCH),
        }
    }

    /// Returns this instant moved later by `d`, saturating at the maximum
    /// representable instant.
    #[must_use]
    pub fn plus(self, d: Duration) -> Self {
        match self.0.checked_add(d) {
            Some(t) => Self(t),
            None => self,
        }
    }
}
