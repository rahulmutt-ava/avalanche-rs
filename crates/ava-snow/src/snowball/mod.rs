// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowball primitives: parameters + slush/snowflake/snowball state machines
//! (specs 06 §2.1–§2.2).
//!
//! Go builds consensus from layered primitives. A *snowflake* tracks confidence
//! as consecutive successful polls (reset to zero on any unsuccessful poll); a
//! *snowball* layers preference-by-popularity on top. All state machines here
//! are pure-integer (no floating point on the consensus path, specs 00 §6.1).

pub mod binary_slush;
pub mod binary_snowball;
pub mod binary_snowflake;
pub mod nnary_snowball;
pub mod nnary_snowflake;
pub mod parameters;
pub mod unary_snowball;
pub mod unary_snowflake;

pub use binary_slush::BinarySlush;
pub use binary_snowball::BinarySnowball;
pub use binary_snowflake::BinarySnowflake;
pub use nnary_snowball::NnarySnowball;
pub use nnary_snowflake::NnarySnowflake;
pub use parameters::{DEFAULT_PARAMETERS, MIN_PERCENT_CONNECTED_BUFFER, Parameters};
pub use unary_snowball::UnarySnowball;
pub use unary_snowflake::UnarySnowflake;

/// One `(alpha_confidence, beta)` termination condition.
///
/// Go generalizes "beta consecutive polls at `alpha_confidence`" into an
/// ascending list of conditions; the default builds a single condition. Each
/// condition has its own confidence counter; finalization occurs when **any**
/// counter reaches its beta.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminationCondition {
    /// Vote threshold to increment this condition's confidence counter.
    pub alpha_confidence: u32,
    /// Consecutive successful polls required to finalize via this condition.
    pub beta: u32,
}

impl TerminationCondition {
    /// Builds a single-condition list (Go `newSingleTerminationCondition`).
    #[must_use]
    pub fn single(alpha_confidence: u32, beta: u32) -> Vec<TerminationCondition> {
        vec![TerminationCondition {
            alpha_confidence,
            beta,
        }]
    }
}
