// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Binary snowflake (specs 06 §2.2; Go `binary_snowflake.go`).

use std::fmt;

use super::TerminationCondition;
use super::binary_slush::BinarySlush;
use super::consensus::BinaryInstance;
use super::fmt_confidence;

/// A binary snowflake instance deciding between two `int` choices (`0`/`1`).
#[derive(Clone, Debug)]
pub struct BinarySnowflake {
    slush: BinarySlush,
    alpha_preference: u32,
    conditions: Vec<TerminationCondition>,
    confidence: Vec<u32>,
    finalized: bool,
}

impl BinarySnowflake {
    /// Builds a binary snowflake preferring `choice`.
    #[must_use]
    pub fn new(alpha_preference: u32, conditions: Vec<TerminationCondition>, choice: u8) -> Self {
        let n = conditions.len();
        Self {
            slush: BinarySlush::new(choice),
            alpha_preference,
            conditions,
            confidence: vec![0; n],
            finalized: false,
        }
    }

    /// Builds a binary snowflake from cloned unary state (Go `unarySnowflake.Extend`).
    #[must_use]
    pub(crate) fn from_parts(
        choice: u8,
        alpha_preference: u32,
        conditions: Vec<TerminationCondition>,
        confidence: Vec<u32>,
        finalized: bool,
    ) -> Self {
        Self {
            slush: BinarySlush::new(choice),
            alpha_preference,
            conditions,
            confidence,
            finalized,
        }
    }

    /// The current preference (`0` or `1`).
    #[must_use]
    pub fn preference(&self) -> u8 {
        self.slush.preference()
    }

    /// Records a poll where `count` nodes preferred `choice`.
    pub fn record_poll(&mut self, count: u32, choice: u8) {
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

    /// Read access to the per-condition confidence counters (used by
    /// `BinarySnowball` for its `String`/state inspection in tests).
    #[must_use]
    pub(crate) fn confidence(&self) -> &[u32] {
        &self.confidence
    }

    /// The current alpha-preference threshold (used by `BinarySnowball`).
    #[must_use]
    pub(crate) fn alpha_preference(&self) -> u32 {
        self.alpha_preference
    }
}

impl fmt::Display for BinarySnowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SF(Confidence = {}, Finalized = {}, {})",
            fmt_confidence(&self.confidence),
            self.finalized,
            self.slush
        )
    }
}

impl BinaryInstance for BinarySnowflake {
    fn preference(&self) -> u8 {
        BinarySnowflake::preference(self)
    }

    fn record_poll(&mut self, count: u32, choice: u8) {
        BinarySnowflake::record_poll(self, count, choice);
    }

    fn record_unsuccessful_poll(&mut self) {
        BinarySnowflake::record_unsuccessful_poll(self);
    }

    fn finalized(&self) -> bool {
        BinarySnowflake::finalized(self)
    }
}
