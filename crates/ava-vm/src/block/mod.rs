// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman VM trait family (specs 07 §2.3–§2.5).
//!
//! * [`Block`] — re-exported from `ava-snow` (the trait is owned by `06`; specs
//!   07 §2.3). `08`–`11` implement it.
//! * [`ChainVm`] — the base Snowman VM (`block.ChainVM`).
//! * [`BuildBlockWithContext`] / [`SetPreferenceWithContext`] — the proposervm
//!   `*WithContext` capabilities + [`BlockContext`] / [`WithVerifyContext`].
//! * [`BatchedChainVm`] + the [`get_ancestors`] / [`batched_parse_block`]
//!   fallbacks.
//! * [`StateSyncableVm`] / [`StateSummary`] / [`StateSyncMode`].

pub mod batched;
pub mod chain_vm;
pub mod state_sync;
pub mod with_context;

// `Block` is owned by `06` and re-exported here so downstream VM crates depend
// only on `ava-vm` for the Snowman VM surface (specs 07 §2.3).
pub use ava_snow::Block;

pub use batched::{BatchedChainVm, INT_LEN, batched_parse_block, get_ancestors};
pub use chain_vm::{BuildBlockWithContext, ChainVm, SetPreferenceWithContext};
pub use state_sync::{StateSummary, StateSyncMode, StateSyncableVm};
pub use with_context::{BlockContext, WithVerifyContext};
