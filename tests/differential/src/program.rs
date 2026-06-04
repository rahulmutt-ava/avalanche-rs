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
