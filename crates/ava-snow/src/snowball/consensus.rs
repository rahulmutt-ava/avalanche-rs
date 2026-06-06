// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowball [`Consensus`] / [`Factory`] traits and the unary/n-nary snow
//! instance traits the [`Tree`](super::tree::Tree) is built from (specs 06
//! §2.3; Go `snow/consensus/snowball/consensus.go`, `factory.go`).
//!
//! These mirror Go's `Consensus`, `Factory`, `Nnary`, and `Unary` interfaces.
//! `Binary` is not a separate trait here: the binary instance is concrete
//! ([`BinarySnowball`](super::binary_snowball::BinarySnowball) /
//! [`BinarySnowflake`](super::binary_snowflake::BinarySnowflake)) and consumed
//! directly by the [`binaryNode`](super::tree) via the `Display`-bearing
//! state-machine API, matching how the tree threads votes down a single bit.

use std::fmt::Display;

use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::Parameters;
use super::binary_snowball::BinarySnowball;
use super::binary_snowflake::BinarySnowflake;
use super::nnary_snowball::NnarySnowball;
use super::nnary_snowflake::NnarySnowflake;
use super::unary_snowball::UnarySnowball;
use super::unary_snowflake::UnarySnowflake;

/// A general snow instance processing the results of network queries (Go
/// `snowball.Consensus`).
pub trait Consensus: Display {
    /// Adds a new choice to vote on.
    fn add(&mut self, choice: Id);

    /// The currently preferred choice to be finalized.
    fn preference(&self) -> Id;

    /// Records the results of a network poll. Assumes all choices have been
    /// previously added. Returns whether the poll was successful (if not
    /// already finalized; once finalized the return value is unspecified, as in
    /// Go).
    fn record_poll(&mut self, votes: &Bag<Id>) -> bool;

    /// Resets the snowflake counters of this consensus instance.
    fn record_unsuccessful_poll(&mut self);

    /// Whether a choice has been finalized.
    fn finalized(&self) -> bool;
}

/// Produces n-nary and unary decision instances (Go `snowball.Factory`).
pub trait Factory {
    /// The unary instance type this factory produces.
    type Unary: UnaryInstance<Binary = Self::Binary>;
    /// The binary instance the unary extends into.
    type Binary: BinaryInstance;
    /// The n-nary instance type this factory produces.
    type Nnary: NnaryInstance;

    /// Builds a new n-nary instance preferring `choice`.
    fn new_nnary(&self, params: Parameters, choice: Id) -> Self::Nnary;

    /// Builds a new unary instance.
    fn new_unary(&self, params: Parameters) -> Self::Unary;
}

/// An n-nary snow instance deciding between an unbounded number of values (Go
/// `snowball.Nnary`).
pub trait NnaryInstance: Display {
    /// Adds a new possible choice.
    fn add(&mut self, choice: Id);

    /// The currently preferred choice to be finalized.
    fn preference(&self) -> Id;

    /// Records the results of a network poll (`count` nodes preferred `choice`).
    fn record_poll(&mut self, count: u32, choice: Id);

    /// Resets the snowflake counter of this instance.
    fn record_unsuccessful_poll(&mut self);

    /// Whether a choice has been finalized.
    fn finalized(&self) -> bool;
}

/// A binary snow instance deciding between two values (Go `snowball.Binary`).
pub trait BinaryInstance: Display {
    /// The currently preferred choice (`0` or `1`).
    fn preference(&self) -> u8;

    /// Records the results of a network poll (`count` nodes preferred `choice`).
    fn record_poll(&mut self, count: u32, choice: u8);

    /// Resets the snowflake counter of this instance.
    fn record_unsuccessful_poll(&mut self);

    /// Whether a choice has been finalized.
    fn finalized(&self) -> bool;
}

/// A unary snow instance deciding on one value (Go `snowball.Unary`).
pub trait UnaryInstance: Display + Clone {
    /// The binary instance this unary extends into.
    type Binary: BinaryInstance;

    /// Records the results of a network poll (`count` nodes preferred the
    /// choice).
    fn record_poll(&mut self, count: u32);

    /// Resets the snowflake counter of this instance.
    fn record_unsuccessful_poll(&mut self);

    /// Whether the value has been finalized.
    fn finalized(&self) -> bool;

    /// Extends into a binary instance preferring `original_preference`.
    fn extend(&self, original_preference: u8) -> Self::Binary;

    /// A clone of this unary instance with the same state (Go `Unary.Clone`).
    fn clone_instance(&self) -> Self;
}

// ----- Snowball factory (the default network factory) -----

/// Produces snowball instances (Go `snowball.SnowballFactory`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SnowballFactory;

impl Factory for SnowballFactory {
    type Unary = UnarySnowball;
    type Binary = BinarySnowball;
    type Nnary = NnarySnowball;

    fn new_nnary(&self, params: Parameters, choice: Id) -> NnarySnowball {
        NnarySnowball::new(
            params.alpha_preference,
            super::TerminationCondition::single(params.alpha_confidence, params.beta),
            choice,
        )
    }

    fn new_unary(&self, params: Parameters) -> UnarySnowball {
        UnarySnowball::new(
            params.alpha_preference,
            super::TerminationCondition::single(params.alpha_confidence, params.beta),
        )
    }
}

/// Produces snowflake instances (Go `snowball.SnowflakeFactory`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SnowflakeFactory;

impl Factory for SnowflakeFactory {
    type Unary = UnarySnowflake;
    type Binary = BinarySnowflake;
    type Nnary = NnarySnowflake;

    fn new_nnary(&self, params: Parameters, choice: Id) -> NnarySnowflake {
        NnarySnowflake::new(
            params.alpha_preference,
            super::TerminationCondition::single(params.alpha_confidence, params.beta),
            choice,
        )
    }

    fn new_unary(&self, params: Parameters) -> UnarySnowflake {
        UnarySnowflake::new(
            params.alpha_preference,
            super::TerminationCondition::single(params.alpha_confidence, params.beta),
        )
    }
}
