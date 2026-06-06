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
pub mod consensus;
pub mod nnary_snowball;
pub mod nnary_snowflake;
pub mod parameters;
pub mod tree;
pub mod unary_snowball;
pub mod unary_snowflake;

pub use binary_slush::BinarySlush;
pub use binary_snowball::BinarySnowball;
pub use binary_snowflake::BinarySnowflake;
pub use consensus::{
    BinaryInstance, Consensus, Factory, NnaryInstance, SnowballFactory, SnowflakeFactory,
    UnaryInstance,
};
pub use nnary_snowball::NnarySnowball;
pub use nnary_snowflake::NnarySnowflake;
pub use parameters::{DEFAULT_PARAMETERS, MIN_PERCENT_CONNECTED_BUFFER, Parameters};
pub use tree::Tree;
pub use unary_snowball::UnarySnowball;
pub use unary_snowflake::UnarySnowflake;

/// Formats a confidence-counter slice like Go's `fmt %v` of a `[]int`:
/// space-separated, bracketed (e.g. `[0]`, `[1 2]`). Used by the `Display`
/// impls that the tree's `String()` golden vectors assert against.
#[must_use]
pub(crate) fn fmt_confidence(confidence: &[u32]) -> String {
    let mut s = String::from("[");
    for (i, c) in confidence.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&c.to_string());
    }
    s.push(']');
    s
}

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
