// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared VM building blocks (`vms/components`, specs 07 §3).
//!
//! * [`avax`] — the UTXO model (UTXO/transferables/metadata/`FlowChecker`) +
//!   atomic shared memory.
//! * [`verify`] — the `Verifiable`/`State` trait family.
//! * [`chain`] — the caching block-state decorator.
//! * [`gas`] — the ACP-103 dynamic-fee primitive (integer-only).

pub mod avax;
pub mod chain;
pub mod gas;
pub mod verify;
