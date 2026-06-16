// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Binary snowball (specs 06 §2.2; Go `binary_snowball.go`).

use std::fmt;

use super::TerminationCondition;
use super::binary_snowflake::BinarySnowflake;
use super::consensus::BinaryInstance;

/// A binary snowball instance: layers preference-by-popularity on a binary
/// snowflake.
#[derive(Clone, Debug)]
pub struct BinarySnowball {
    snowflake: BinarySnowflake,
    /// The choice with the largest number of polls preferring it. Ties break by
    /// switching choice lazily.
    preference: u8,
    /// Total polls that preferred each choice (`[choice 0, choice 1]`).
    preference_strength: [u32; 2],
}

impl BinarySnowball {
    /// Builds a binary snowball preferring `choice`.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>, choice: u8) -> Self {
        Self {
            snowflake: BinarySnowflake::new(alpha_preference, conditions, choice),
            preference: choice,
            preference_strength: [0, 0],
        }
    }

    /// Builds a binary snowball from an extended unary snowflake (Go
    /// `unarySnowball.Extend`): `preference = choice` and
    /// `preference_strength[choice] = unary_strength`.
    #[must_use]
    pub(crate) fn from_extension(
        snowflake: BinarySnowflake,
        choice: u8,
        unary_strength: u32,
    ) -> Self {
        let mut preference_strength = [0u32; 2];
        preference_strength[choice as usize] = unary_strength;
        Self {
            snowflake,
            preference: choice,
            preference_strength,
        }
    }

    /// The current preference. If the snowflake has finalized, its (finalized)
    /// choice is preferred; otherwise the popularity-leading choice.
    #[must_use]
    pub fn preference(&self) -> u8 {
        if self.snowflake.finalized() {
            return self.snowflake.preference();
        }
        self.preference
    }

    /// Records a poll where `count` nodes preferred `choice`.
    pub fn record_poll(&mut self, count: u32, choice: u8) {
        if count >= self.snowflake.alpha_preference() {
            let idx = choice as usize;
            self.preference_strength[idx] = self.preference_strength[idx].saturating_add(1);
            // `idx` is 0 or 1 (binary), so `idx ^ 1` is the opposite index.
            if self.preference_strength[idx] > self.preference_strength[idx ^ 1] {
                self.preference = choice;
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

    /// The per-condition confidence of the underlying snowflake (test
    /// inspection).
    #[must_use]
    pub fn confidence(&self) -> &[u32] {
        self.snowflake.confidence()
    }

    /// The per-choice preference strength (test inspection).
    #[must_use]
    pub fn preference_strength(&self) -> [u32; 2] {
        self.preference_strength
    }
}

impl fmt::Display for BinarySnowball {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SB(Preference = {}, PreferenceStrength[0] = {}, PreferenceStrength[1] = {}, {})",
            self.preference,
            self.preference_strength[0],
            self.preference_strength[1],
            self.snowflake
        )
    }
}

impl BinaryInstance for BinarySnowball {
    fn preference(&self) -> u8 {
        BinarySnowball::preference(self)
    }

    fn record_poll(&mut self, count: u32, choice: u8) {
        BinarySnowball::record_poll(self, count, choice);
    }

    fn record_unsuccessful_poll(&mut self) {
        BinarySnowball::record_unsuccessful_poll(self);
    }

    fn finalized(&self) -> bool {
        BinarySnowball::finalized(self)
    }
}
