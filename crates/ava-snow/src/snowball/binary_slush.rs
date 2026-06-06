// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Binary slush (specs 06 §2.2; Go `binary_slush.go`).

use std::fmt;

/// A binary slush instance: tracks the last choice with a successful poll.
#[derive(Clone, Copy, Debug)]
pub struct BinarySlush {
    /// The choice (`0` or `1`) that last had a successful poll, or the initial
    /// choice if none has occurred.
    pub(crate) preference: u8,
}

impl BinarySlush {
    /// Builds a binary slush preferring `choice`.
    #[must_use]
    pub fn new(choice: u8) -> Self {
        Self { preference: choice }
    }

    /// The current preference.
    #[must_use]
    pub fn preference(&self) -> u8 {
        self.preference
    }

    /// Adopts `choice` as the preference (a successful poll).
    pub fn record_successful_poll(&mut self, choice: u8) {
        self.preference = choice;
    }
}

impl fmt::Display for BinarySlush {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SL(Preference = {})", self.preference)
    }
}
