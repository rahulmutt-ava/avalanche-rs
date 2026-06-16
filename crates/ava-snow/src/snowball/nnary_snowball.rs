// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! N-nary snowball (specs 06 §2.2; Go `nnary_snowball.go`).

use std::collections::BTreeMap;
use std::fmt;

use ava_types::id::Id;

use super::TerminationCondition;
use super::consensus::NnaryInstance;
use super::nnary_snowflake::NnarySnowflake;

/// An n-nary snowball instance: layers preference-by-popularity on an n-nary
/// snowflake.
#[derive(Clone, Debug)]
pub struct NnarySnowball {
    snowflake: NnarySnowflake,
    /// The choice with the largest number of polls preferring it. Ties break by
    /// switching choice lazily.
    preference: Id,
    /// The maximum value stored in `preference_strength`.
    max_preference_strength: u32,
    /// Total polls preferring each choice. A `BTreeMap` (not `HashMap`) keeps the
    /// ordered/deterministic-iteration contract (specs 00 §6.1); the decision
    /// logic itself is order-independent because the leader is tracked
    /// incrementally.
    preference_strength: BTreeMap<Id, u32>,
}

impl NnarySnowball {
    /// Builds an n-nary snowball preferring `choice`.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>, choice: Id) -> Self {
        Self {
            snowflake: NnarySnowflake::new(alpha_preference, conditions, choice),
            preference: choice,
            max_preference_strength: 0,
            preference_strength: BTreeMap::new(),
        }
    }

    /// Adds a new possible choice (delegates to the snowflake; Go `Add`).
    pub fn add(&mut self, choice: Id) {
        self.snowflake.add(choice);
    }

    /// The current preference. If the snowflake has finalized, its (finalized)
    /// choice is preferred; otherwise the popularity-leading choice.
    #[must_use]
    pub fn preference(&self) -> Id {
        if self.snowflake.finalized() {
            return self.snowflake.preference();
        }
        self.preference
    }

    /// The snowflake-layer preference (test inspection; Go
    /// `sb.nnarySnowflake.Preference()`).
    #[must_use]
    pub fn snowflake_preference(&self) -> Id {
        self.snowflake.preference()
    }

    /// Records a poll where `count` nodes preferred `choice`.
    pub fn record_poll(&mut self, count: u32, choice: Id) {
        if count >= self.snowflake.alpha_preference() {
            let strength = self.preference_strength.entry(choice).or_insert(0);
            *strength = strength.saturating_add(1);
            let strength = *strength;
            if strength > self.max_preference_strength {
                self.preference = choice;
                self.max_preference_strength = strength;
            }
        }
        self.snowflake.record_poll(count, choice);
    }

    /// Records an unsuccessful poll on the underlying snowflake.
    pub fn record_unsuccessful_poll(&mut self) {
        self.snowflake.record_unsuccessful_poll();
    }

    /// Whether this instance has finalized.
    #[must_use]
    pub fn finalized(&self) -> bool {
        self.snowflake.finalized()
    }

    /// The underlying snowflake's per-condition confidence (test inspection).
    #[must_use]
    pub fn confidence(&self) -> &[u32] {
        self.snowflake.confidence()
    }

    /// The maximum preference strength (test inspection).
    #[must_use]
    pub fn max_preference_strength(&self) -> u32 {
        self.max_preference_strength
    }
}

impl fmt::Display for NnarySnowball {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SB(Preference = {}, PreferenceStrength = {}, {})",
            self.preference, self.max_preference_strength, self.snowflake
        )
    }
}

impl NnaryInstance for NnarySnowball {
    fn add(&mut self, choice: Id) {
        NnarySnowball::add(self, choice);
    }

    fn preference(&self) -> Id {
        NnarySnowball::preference(self)
    }

    fn record_poll(&mut self, count: u32, choice: Id) {
        NnarySnowball::record_poll(self, count, choice);
    }

    fn record_unsuccessful_poll(&mut self) {
        NnarySnowball::record_unsuccessful_poll(self);
    }

    fn finalized(&self) -> bool {
        NnarySnowball::finalized(self)
    }
}
