// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain validator subsystem (`vms/platformvm/validators`).
//!
//! Hosts the ACP-77 L1 validator continuous-fee mechanism ([`fee`]) and the
//! [`PChainValidatorManager`](manager::PChainValidatorManager) — the P-Chain
//! implementation of [`ava_validators::ValidatorState`] with backward
//! diff-windowing validator-set reconstruction (M4.21, specs 08 §7).

pub mod fee;
pub mod manager;

pub use manager::PChainValidatorManager;
