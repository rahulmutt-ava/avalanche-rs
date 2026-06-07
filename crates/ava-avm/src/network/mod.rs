// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! X-Chain (AVM) network layer — gossip handler, tx-verifier seam, and atomic
//! app-handler switch (specs 09 §8; `vms/avm/network/`).
//!
//! ## Scope (M5.18, deferred-transport seam)
//!
//! The generic p2p gossip framework (specs 05 / M2: push/pull Bloom-Set
//! reconciliation, `Gossipable` trait, `Gossiper`/`Marshaller`) does **not**
//! yet exist in `ava-network`. This module implements the VM-side handler
//! logic — the admission policy, the verifier seam, and the atomic switch —
//! which is sufficient for the M5 exit gate. Wiring these to the real
//! transport is the 05/M2 follow-up.
//!
//! ## Deferred (05/M2 follow-up)
//!
//! * Generic push/pull gossip transport (Bloom-Set, `Gossiper`, peer fan-out).
//! * Wiring `AtomicAppHandler` into the VM's `AppHandler::app_gossip` (M5.19).

pub mod atomic;
pub mod gossip;
pub mod tx_verifier;

pub use atomic::{AppGossipHandler, AtomicAppHandler};
pub use gossip::{DropReason, Gossipable, HandleOutcome, TxGossipHandler, TxMarshaller};
pub use tx_verifier::{SemanticTxVerifier, SyntacticTxVerifier, TxVerifier};
