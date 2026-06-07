// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain tx executor — verification + state application (specs 09 §6).
//!
//! Port of `vms/avm/txs/executor`. The verification pipeline is split into a
//! stateless [`syntactic`] pass and (later) a stateful semantic pass:
//!
//! * [`backend::Backend`] — the shared verification context (chain ids, fees,
//!   fx count) consumed by both passes.
//! * [`syntactic::SyntacticVerifier`] — the `txs.Visitor` that validates a parsed
//!   `Tx` without chain state (M5.12).
//!
//! The `SemanticVerifier` (M5.13) and the state-applying `Executor` (M5.14) land
//! in later tasks and slot in alongside these modules.

pub mod backend;
pub mod syntactic;

pub use backend::{Backend, Config};
pub use syntactic::SyntacticVerifier;
