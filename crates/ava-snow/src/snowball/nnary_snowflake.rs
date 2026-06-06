// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! N-nary snowflake + slush (specs 06 §2.2; Go `nnary_snowflake.go`,
//! `nnary_slush.go`).

use ava_types::id::Id;

use super::TerminationCondition;

/// An n-nary slush instance over an unbounded number of choices.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NnarySlush {
    preference: Id,
}

impl NnarySlush {
    fn new(choice: Id) -> Self {
        Self { preference: choice }
    }

    fn preference(&self) -> Id {
        self.preference
    }

    fn record_successful_poll(&mut self, choice: Id) {
        self.preference = choice;
    }
}

/// An n-nary snowflake instance: a slush layer with an `alpha_preference` gate
/// and per-condition confidence counters.
#[derive(Clone, Debug)]
pub struct NnarySnowflake {
    slush: NnarySlush,
    alpha_preference: u32,
    conditions: Vec<TerminationCondition>,
    confidence: Vec<u32>,
    finalized: bool,
}

impl NnarySnowflake {
    /// Builds an n-nary snowflake preferring `choice`.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>, choice: Id) -> Self {
        let n = conditions.len();
        Self {
            slush: NnarySlush::new(choice),
            alpha_preference,
            conditions,
            confidence: vec![0; n],
            finalized: false,
        }
    }

    /// Adds a new possible choice (a no-op on the snowflake; Go `Add`).
    pub fn add(&mut self, _choice: Id) {}

    /// The current preference.
    #[must_use]
    pub fn preference(&self) -> Id {
        self.slush.preference()
    }

    /// Records a poll where `count` nodes preferred `choice`.
    pub fn record_poll(&mut self, count: u32, choice: Id) {
        if self.finalized {
            return; // Already decided.
        }
        if count < self.alpha_preference {
            self.record_unsuccessful_poll();
            return;
        }
        // Changing preference resets confidence before the slush update.
        if choice != self.preference() {
            self.confidence.fill(0);
        }
        self.slush.record_successful_poll(choice);

        for i in 0..self.conditions.len() {
            if count < self.conditions[i].alpha_confidence {
                self.confidence[i..].fill(0);
                return;
            }
            self.confidence[i] += 1;
            if self.confidence[i] >= self.conditions[i].beta {
                self.finalized = true;
                return;
            }
        }
    }

    /// Records an unsuccessful poll, clearing all confidence.
    pub fn record_unsuccessful_poll(&mut self) {
        self.confidence.fill(0);
    }

    /// Whether this instance has finalized.
    #[must_use]
    pub fn finalized(&self) -> bool {
        self.finalized
    }

    /// The per-condition confidence counters (test inspection).
    #[must_use]
    pub(crate) fn confidence(&self) -> &[u32] {
        &self.confidence
    }

    /// The alpha-preference threshold (used by `NnarySnowball`).
    #[must_use]
    pub(crate) fn alpha_preference(&self) -> u32 {
        self.alpha_preference
    }
}
