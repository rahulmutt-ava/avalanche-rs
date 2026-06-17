// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-snow` — the Snowball/Snowman consensus core.
//!
//! This crate is the determinism-critical heart of Avalanche consensus. It is a
//! byte-/transition-exact port of avalanchego's `snow/` tree (specs
//! `06-consensus.md`). The public surface, built up across milestone M3:
//!
//! - [`Parameters`](snowball::Parameters) + `verify()` and the
//!   slush/snowflake/snowball primitives ([`snowball`]).
//! - The consensus [`context`] (`ChainContext`/`ConsensusContext`), the
//!   [`Block`] trait, [`Status`], [`EngineState`]/[`EngineType`], the
//!   [`Acceptor`] callback, and the crate [`Error`]/[`Result`] model.
//!
//! Determinism rules (specs 00 §6.1) are enforced throughout: no `HashMap` on
//! ordered/serialization paths, no floating-point in consensus math, checked
//! arithmetic, and an injected clock in tests.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Determinism hardening (spec 24 §A.2, hazard #2/#3; X.19 follow-up): no
// floating-point and no unchecked integer arithmetic on consensus paths.
#![deny(clippy::arithmetic_side_effects)]
#![deny(clippy::float_arithmetic)]
// X.19 follow-up: surface lossy narrowing casts on consensus paths. Library
// sites use checked conversions; test code legitimately casts bounded values.
#![warn(clippy::cast_possible_truncation)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

pub mod acceptor;
pub mod choices;
pub mod context;
pub mod decidable;
pub mod error;
pub mod snowball;
pub mod snowman;
pub mod state;

#[cfg(feature = "testutil")]
pub mod snowtest;

#[cfg(feature = "testutil")]
pub mod testutil;

pub use acceptor::{Acceptor, NoOpAcceptor};
pub use choices::Status;
pub use context::{ChainContext, ConsensusContext};
pub use decidable::Block;
pub use error::{Error, Result};
pub use state::{EngineState, EngineType};

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire/persisted enum values must match Go `choices.Status` exactly.
    #[test]
    fn status_wire_values() {
        assert_eq!(Status::Unknown as u8, 0);
        assert_eq!(Status::Processing as u8, 1);
        assert_eq!(Status::Rejected as u8, 2);
        assert_eq!(Status::Accepted as u8, 3);

        assert!(!Status::Unknown.valid());
        assert!(Status::Processing.valid());
        assert!(Status::Accepted.decided());
        assert!(Status::Rejected.decided());
        assert!(!Status::Processing.decided());
    }

    /// `Arc<ChainContext>` must be `Send + Sync` so it can be threaded into VMs
    /// and engine tasks across threads (specs 06 §3).
    #[test]
    fn chain_context_is_send_sync() {
        fn _assert<T: Send + Sync>() {}
        _assert::<std::sync::Arc<ChainContext>>();
        _assert::<std::sync::Arc<ConsensusContext>>();
    }
}
