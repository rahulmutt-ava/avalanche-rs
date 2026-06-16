// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The differential program: a seed-derived sequence of [`Action`]s (specs/02 §11.2).

use arbitrary::Arbitrary;

/// One step a differential program can take against a node under test.
///
/// SCAFFOLD: the variants mirror specs/02 §11.2; their payloads (tx bytes, API
/// requests, partition specs) are derived deterministically from the program
/// seed by the [`crate::LockstepDriver`] in tier-X task X.13.
#[derive(Debug, Clone, PartialEq, Eq, Arbitrary)]
pub enum Action {
    /// Issue a (seed-derived) transaction.
    IssueTx,
    /// Make a (seed-derived) API call and capture the normalized response.
    ApiCall,
    /// Advance the injected clock by a bounded amount.
    AdvanceTime,
    /// Restart a node (crash-recovery exercise).
    RestartNode,
    /// Partition the network, then heal it.
    Partition,
    /// Await finalization and capture an [`crate::Observation`].
    AwaitFinalization,
}

/// A bounded program of [`Action`]s with the seed that generated it.
#[derive(Debug, Clone, PartialEq, Eq, Arbitrary)]
pub struct Program {
    /// The seed every deterministic derivation (tx/key bytes, sampler RNG) flows from.
    pub seed: u64,
    /// The ordered actions to replay.
    pub actions: Vec<Action>,
}

impl Program {
    /// Generate a small, bounded program deterministically from `seed`.
    ///
    /// The program shape (action count and which variant each slot is) is a pure
    /// function of `seed` via a tiny splitmix-style bit mixer, so two calls with
    /// the same seed produce an identical [`Program`] (specs/00 §6.1). The shape
    /// is intentionally simple: a handful of `IssueTx`/`AdvanceTime`/`ApiCall`
    /// steps interleaved with `AwaitFinalization`s, with a trailing
    /// `AwaitFinalization` guaranteeing at least one finalization point so the
    /// replay always captures an observation.
    #[must_use]
    pub fn from_seed(seed: u64) -> Program {
        // Action count: 2..=9 (a 3-bit mask, no overflow), then +2 so there is
        // always work plus a finalization.
        let count = (mix(seed) & 0x7).saturating_add(2);
        let mut actions = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for i in 0..count {
            // Derive each slot's variant from a per-slot mix of the seed. Every
            // 3rd slot is an AwaitFinalization so the replay captures multiple
            // observations; the rest cycle through the issuance/clock/api actions.
            let m = mix(seed.wrapping_add(i.wrapping_mul(0x9E37_79B9)));
            let action = if i % 3 == 2 {
                Action::AwaitFinalization
            } else {
                match m % 3 {
                    0 => Action::IssueTx,
                    1 => Action::AdvanceTime,
                    _ => Action::ApiCall,
                }
            };
            actions.push(action);
        }
        // Always end on a finalization so at least one observation is captured.
        actions.push(Action::AwaitFinalization);
        Program { seed, actions }
    }
}

/// A tiny deterministic bit-mixer (splitmix64 finalizer) — pure, no global state.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}
