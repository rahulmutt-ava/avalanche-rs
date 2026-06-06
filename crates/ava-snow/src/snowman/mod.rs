// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowman: linear-chain consensus (specs 06 §2.4).
//!
//! [`Topological`] is the production implementation, deciding a single chain of
//! blocks by tracking the strongly preferred branch with a tree of snowball
//! instances. The [`SnowmanConsensus`] trait is the interface the engine drives;
//! [`Block`]/[`BlockAcceptor`] are the synchronous consensus-internal block
//! interfaces.

pub mod block;
pub mod consensus;
pub mod topological;

pub use block::{Block, BlockAcceptor, NoOpBlockAcceptor};
pub use consensus::SnowmanConsensus;
pub use topological::Topological;
