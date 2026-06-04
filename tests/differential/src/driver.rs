// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The lockstep driver (specs/02 §11.2/§11.5).

use crate::program::Program;

/// Replays a [`Program`] against a node under test, deriving every tx/key byte
/// deterministically from the program seed and feeding the same seed to the
/// sampler RNG (specs/00 §6.1), then collecting an [`crate::Observation`] at
/// each `AwaitFinalization`.
///
/// SCAFFOLD: construction + seed plumbing are sketched; the replay loop,
/// recorded-oracle comparison, and seed-repro (`DIFFERENTIAL_SEED`) are filled
/// in by tier-X task X.13.
#[derive(Debug)]
pub struct LockstepDriver {
    seed: u64,
}

impl LockstepDriver {
    /// Create a driver pinned to a program seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// The seed every deterministic derivation in this run flows from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Replay a program (recorded-oracle mode).
    ///
    /// SCAFFOLD: owned by tier-X task X.13.
    pub fn replay_recorded(&self, _program: &Program) -> Result<(), String> {
        Err("LockstepDriver::replay_recorded is owned by tier-X task X.13".to_owned())
    }
}
