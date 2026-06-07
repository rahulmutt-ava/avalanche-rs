// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain tx executor — verification + state application (specs 09 §6).
//!
//! Port of `vms/avm/txs/executor`. The verification pipeline is split into a
//! stateless [`syntactic`] pass and a stateful [`semantic`] pass:
//!
//! * [`backend::Backend`] — the shared verification context (chain ids, fees,
//!   fx count) consumed by both passes.
//! * [`syntactic::SyntacticVerifier`] — the `txs.Visitor` that validates a parsed
//!   `Tx` without chain state (M5.12).
//! * [`semantic::SemanticVerifier`] — the `txs.Visitor` that validates a parsed
//!   `Tx` against chain state (input UTXOs + the asset's `CreateAssetTx`),
//!   including the `SameSubnet` gate and the grandfathered-op bug-compat quirk
//!   ([`consts::GRANDFATHERED_OPERATION_TX`]) (M5.13).
//!
//! The state-applying `Executor` (M5.14) lands in a later task and slots in
//! alongside these modules.

pub mod backend;
pub mod consts;
pub mod semantic;
pub mod syntactic;

pub use backend::{Backend, Config};
pub use consts::GRANDFATHERED_OPERATION_TX;
pub use semantic::SemanticVerifier;
pub use syntactic::SyntacticVerifier;
