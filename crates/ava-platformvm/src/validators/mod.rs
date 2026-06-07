// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain validator subsystem (`vms/platformvm/validators`).
//!
//! Currently hosts the ACP-77 L1 validator continuous-fee mechanism
//! ([`fee`]); the validator-set / windowing machinery lands in later M4 tasks.

pub mod fee;
