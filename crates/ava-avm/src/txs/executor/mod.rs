// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain tx executor — verification + state application (specs 09 §6).
//!
//! Port of `vms/avm/txs/executor`. The verification pipeline is split into a
//! stateless [`syntactic`] pass and a stateful [`semantic`] pass, followed by
//! the state-applying [`exec`] pass:
//!
//! * [`backend::Backend`] — the shared verification context (chain ids, fees,
//!   fx count) consumed by both passes.
//! * [`syntactic::SyntacticVerifier`] — the `txs.Visitor` that validates a parsed
//!   `Tx` without chain state (M5.12).
//! * [`semantic::SemanticVerifier`] — the `txs.Visitor` that validates a parsed
//!   `Tx` against chain state (input UTXOs + the asset's `CreateAssetTx`),
//!   including the `SameSubnet` gate and the grandfathered-op bug-compat quirk
//!   ([`consts::GRANDFATHERED_OPERATION_TX`]) (M5.13).
//! * [`exec::Executor`] — the state-applying executor: applies a verified
//!   `UnsignedTx` to a `Chain` diff, recording atomic requests for block accept
//!   (M5.14, EXEC-AVM-1, ATOMIC-1).

pub mod backend;
pub mod consts;
pub mod exec;
pub mod semantic;
pub mod syntactic;

pub use backend::{Backend, Config};
pub use consts::GRANDFATHERED_OPERATION_TX;
pub use exec::{Executor, ExecutorOutputs};
pub use semantic::SemanticVerifier;
pub use syntactic::SyntacticVerifier;
