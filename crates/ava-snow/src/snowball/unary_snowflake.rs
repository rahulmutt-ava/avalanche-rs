// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Unary snowflake (specs 06 §2.2; Go `unary_snowflake.go`).

use std::fmt;

use super::TerminationCondition;
use super::binary_snowflake::BinarySnowflake;
use super::consensus::UnaryInstance;
use super::fmt_confidence;

/// A unary snowflake instance: deciding on a single value (the no-conflict
/// case).
///
/// Invariants (matching Go): `conditions` are ascending by `alpha_confidence`,
/// `beta` is descending, and `confidence[i] >= confidence[i+1]` (except after
/// early finalization).
#[derive(Clone, Debug)]
pub struct UnarySnowflake {
    /// Threshold required to update the preference.
    pub(crate) alpha_preference: u32,
    /// Ascending-by-`alpha_confidence` termination conditions.
    pub(crate) conditions: Vec<TerminationCondition>,
    /// Per-condition count of consecutive successful polls.
    pub(crate) confidence: Vec<u32>,
    /// Set once a confidence counter reaches its beta.
    pub(crate) finalized: bool,
}

impl UnarySnowflake {
    /// Builds a unary snowflake with the given termination conditions.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>) -> Self {
        let n = conditions.len();
        Self {
            alpha_preference,
            conditions,
            confidence: vec![0; n],
            finalized: false,
        }
    }

    /// Records a poll where `count` nodes preferred the choice.
    pub fn record_poll(&mut self, count: u32) {
        for i in 0..self.conditions.len() {
            // Did not reach this alpha threshold ⇒ did not reach any higher;
            // clear the remaining confidence counters.
            if count < self.conditions[i].alpha_confidence {
                self.confidence[i..].fill(0);
                return;
            }
            // Reached this threshold: increment and check finalization.
            self.confidence[i] = self.confidence[i].saturating_add(1);
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

    /// The per-condition confidence counters (matches Go `unarySnowflake`
    /// `confidence`; used by golden state-transition vectors).
    #[must_use]
    pub fn confidence(&self) -> &[u32] {
        &self.confidence
    }

    /// Extends this unary instance into a binary one rooted at `choice`,
    /// cloning the accumulated confidence (Go `Extend`).
    #[must_use]
    pub fn extend(&self, choice: u8) -> BinarySnowflake {
        BinarySnowflake::from_parts(
            choice,
            self.alpha_preference,
            self.conditions.clone(),
            self.confidence.clone(),
            self.finalized,
        )
    }
}

impl fmt::Display for UnarySnowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SF(Confidence = {}, Finalized = {})",
            fmt_confidence(&self.confidence),
            self.finalized
        )
    }
}

impl UnaryInstance for UnarySnowflake {
    type Binary = BinarySnowflake;

    fn record_poll(&mut self, count: u32) {
        UnarySnowflake::record_poll(self, count);
    }

    fn record_unsuccessful_poll(&mut self) {
        UnarySnowflake::record_unsuccessful_poll(self);
    }

    fn finalized(&self) -> bool {
        UnarySnowflake::finalized(self)
    }

    fn extend(&self, original_preference: u8) -> BinarySnowflake {
        UnarySnowflake::extend(self, original_preference)
    }

    fn clone_instance(&self) -> Self {
        self.clone()
    }
}
