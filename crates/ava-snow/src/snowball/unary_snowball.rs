// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Unary snowball (specs 06 §2.2; Go `unary_snowball.go`).

use super::TerminationCondition;
use super::binary_snowball::BinarySnowball;
use super::binary_snowflake::BinarySnowflake;
use super::unary_snowflake::UnarySnowflake;

/// A unary snowball instance: a unary snowflake plus a poll-count preference
/// strength.
#[derive(Clone, Debug)]
pub struct UnarySnowball {
    snowflake: UnarySnowflake,
    /// Total number of polls that met `alpha_preference`.
    preference_strength: u32,
}

impl UnarySnowball {
    /// Builds a unary snowball with the given termination conditions.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>) -> Self {
        Self {
            snowflake: UnarySnowflake::new(alpha_preference, conditions),
            preference_strength: 0,
        }
    }

    /// Records a poll where `count` nodes preferred the choice.
    pub fn record_poll(&mut self, count: u32) {
        if count >= self.snowflake.alpha_preference {
            self.preference_strength += 1;
        }
        self.snowflake.record_poll(count);
    }

    /// Records an unsuccessful poll, clearing snowflake confidence.
    pub fn record_unsuccessful_poll(&mut self) {
        self.snowflake.record_unsuccessful_poll();
    }

    /// Whether this instance has finalized.
    #[must_use]
    pub fn finalized(&self) -> bool {
        self.snowflake.finalized()
    }

    /// The accumulated preference strength (test inspection).
    #[must_use]
    pub fn preference_strength(&self) -> u32 {
        self.preference_strength
    }

    /// The underlying snowflake's per-condition confidence (test inspection).
    #[must_use]
    pub fn confidence(&self) -> &[u32] {
        &self.snowflake.confidence
    }

    /// Extends this unary instance into a binary snowball rooted at `choice`,
    /// carrying the accumulated confidence and preference strength (Go `Extend`).
    #[must_use]
    pub fn extend(&self, choice: u8) -> BinarySnowball {
        let snowflake = BinarySnowflake::from_parts(
            choice,
            self.snowflake.alpha_preference,
            self.snowflake.conditions.clone(),
            self.snowflake.confidence.clone(),
            self.snowflake.finalized(),
        );
        BinarySnowball::from_extension(snowflake, choice, self.preference_strength)
    }
}
