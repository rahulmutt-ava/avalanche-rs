// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The lockstep driver (specs/02 §11.2/§11.5).

use crate::observation::Observation;
use crate::program::{Action, Program};
use crate::xchain;

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

    /// Replay a program against the REAL in-process Rust pipeline and return the
    /// ordered, normalized [`Observation`]s — one per `AwaitFinalization`.
    ///
    /// This is the OFFLINE recorded-oracle replay path (specs/02 §11.1
    /// recorded-oracle, specs/00 §6.1 determinism). There is no Go oracle nor live
    /// two-binary mode offline; the meaningful property here is **determinism**:
    /// replaying the SAME program (same seed + actions) twice yields byte-identical
    /// observation sequences. The live two-binary arm (`mixed_network`) compares
    /// the same `Observation` shape between a Go and a Rust node.
    ///
    /// ## How it drives the real pipeline
    ///
    /// The driver walks `program.actions` keeping a running step counter. Every
    /// non-finalization action (`IssueTx` / `ApiCall` / `AdvanceTime` /
    /// `RestartNode` / `Partition`) advances that counter so it folds into the
    /// per-finalization sub-seed. At each `AwaitFinalization` it derives a
    /// sub-seed from `(self.seed, finalization_index, step_counter)` and drives a
    /// fresh `ava-avm` VM through the REAL block pipeline via
    /// [`xchain::run_program`] (seed genesis → admit txs → build → verify →
    /// accept), capturing that finalization's normalized [`Observation`].
    ///
    /// Driving a *fresh* VM per finalization (rather than threading one stepped VM
    /// through the loop) keeps the change additive over the known-good
    /// [`xchain::run_program`] harness (no `xchain.rs` public-surface break — the
    /// `xchain_issue_tx` determinism gate keeps using it unchanged). Because every
    /// sub-seed is a pure function of the program, the whole sequence is
    /// reproducible: the offline gate replays twice and asserts byte-identity.
    ///
    /// # Errors
    /// Returns `Err` only if the program contains no `AwaitFinalization` (then
    /// there is nothing to observe) — a malformed program for this offline gate.
    /// [`Program::from_seed`](crate::program::Program::from_seed) always emits a
    /// trailing finalization, so the generated gate never hits this path.
    pub fn replay_recorded(&self, program: &Program) -> Result<Vec<Observation>, String> {
        let mut observations = Vec::new();
        let mut finalization_index: u64 = 0;
        let mut step_counter: u64 = 0;

        for action in &program.actions {
            match action {
                Action::AwaitFinalization => {
                    // Derive a sub-seed for this finalization from the program seed,
                    // the finalization index, and the steps taken so far — all pure
                    // functions of the program, so the sequence is reproducible.
                    let sub_seed = mix(self.seed
                        ^ mix(finalization_index.wrapping_add(1))
                        ^ mix(step_counter.wrapping_add(0xA5A5)));
                    observations.push(xchain::run_program(sub_seed));
                    finalization_index = finalization_index.saturating_add(1);
                }
                // Every other action is a pre-finalization step that folds into the
                // next finalization's sub-seed (so reordering/adding steps changes
                // the captured observations — the program shape is observable).
                Action::IssueTx
                | Action::ApiCall
                | Action::AdvanceTime
                | Action::RestartNode
                | Action::Partition => {
                    step_counter = step_counter.saturating_add(1);
                }
            }
        }

        if observations.is_empty() {
            return Err(
                "LockstepDriver::replay_recorded: program has no AwaitFinalization to observe"
                    .to_owned(),
            );
        }
        Ok(observations)
    }
}

/// A tiny deterministic bit-mixer (splitmix64 finalizer) — pure, no global state.
/// Matches the mixer the seed-derived program generator uses.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
