// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-types` — core Avalanche primitive types.
//!
//! Tier T0 (primitives). Owning spec: `specs/03-core-primitives.md` §1, §7.
//! Implemented across milestone M0 (see `plan/M0-foundations.md`):
//!
//! - [`id`] / [`short_id`] / [`node_id`] — fixed-length id newtypes (M0.5, M0.6)
//! - [`bits`] — consensus-affecting bit-subset helpers (M0.5)
//! - [`request_id`] / [`aliaser`] — request identity + id aliasing (M0.7)
//! - [`constants`] — network ids + HRPs (M0.8)
//! - [`error`] — the crate error enum (M0.5)
//!
//! NOTE: this crate is the lowest primitive layer and depends on no other
//! `ava-*` crate (specs/03 §0). Modules below are scaffolded empty in M0.1 and
//! filled in by their owning tasks.

#![forbid(unsafe_code)]

pub mod aliaser;
pub mod bits;
pub mod constants;
pub mod error;
pub mod id;
pub mod node_id;
pub mod request_id;
pub mod short_id;
