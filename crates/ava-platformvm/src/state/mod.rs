// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain on-disk state (`vms/platformvm/state`, specs 08 §3).
//!
//! Populated tier-by-tier across the M4 wave plan. The validator-metadata codec
//! (§3.4) landed first: [`metadata_validator::ValidatorMetadata`] and its
//! three-version [`metadata_codec`]. M4.13 adds the [`Chain`]/[`Versions`] trait
//! stack, the persisted [`State`] base over flat-KV [`prefixes`], the in-memory
//! [`Stakers`] collections, and the [`Diff`] overlay.

pub mod chain;
pub mod diff;
pub mod l1_validator;
pub mod metadata_codec;
pub mod metadata_validator;
pub mod prefixes;
pub mod staker;
pub mod stakers;
// The persisted base lives in `state.rs` (the plan-mandated filename), which
// trips `clippy::module_inception` against the parent `state` module.
#[allow(clippy::module_inception)]
pub mod state;

pub use chain::{Chain, UtxoBytes, Versions};
pub use diff::Diff;
pub use stakers::Stakers;
pub use state::State;
