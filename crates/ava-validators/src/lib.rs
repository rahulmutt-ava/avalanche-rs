// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Validator subsystem (`snow/validators`): the per-subnet validator [`Set`], the
//! [`ValidatorManager`] trait + default impl, deterministic weighted sampling that
//! reuses the M0 sampler, the [`ValidatorState`] trait + cached/locked adapters,
//! and the [`ConnectedValidators`] connectivity tracker.
//!
//! # Determinism
//! The sampling/serialization path is canonically `NodeId`-ordered: [`Set`] keys
//! validators in a `BTreeMap` and [`ValidatorState::get_validator_set`] returns a
//! `BTreeMap`. Any caller that samples or serializes a validator set iterates it
//! in `NodeId` order — exactly where Go calls `utils.Sort`. The proposervm windower
//! depends on this canonical order (`specs/06-consensus.md` §6.1, §6.2).
//!
//! This crate depends only on `ava-types`, `ava-crypto`, and `ava-utils`; it does
//! **not** depend on `ava-snow` (`Id`/`NodeId` live in `ava-types`).

#![forbid(unsafe_code)]

pub mod connected;
pub mod error;
pub mod manager;
pub mod set;
pub mod state;
pub mod state_adapters;
pub mod validator;

pub use connected::{ConnectedValidators, Connector};
pub use error::{Error, Result};
pub use manager::{DefaultManager, ManagerCallbackListener, ValidatorManager};
pub use set::Set;
pub use state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
pub use state_adapters::{CachedState, LockedState};
pub use validator::{GetValidatorOutput, Validator};
