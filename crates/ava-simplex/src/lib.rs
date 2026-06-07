// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-simplex` — the Simplex BFT consensus integration (port of `simplex/` +
//! `snow/consensus/simplex/parameters.go`, specs 06 §8).
//!
//! Simplex is an alternative single-decree-per-round BFT consensus
//! (propose → vote → finalize with quorum certificates), offered as a pluggable
//! consensus for subnets that want BFT finality over a fixed validator set
//! instead of Snowman's metastable sampling. Finality is a **BLS quorum** (⅔ of
//! the validator set), so there is no Snowball `k`/`alpha`/`beta` schedule.
//!
//! This crate ports the always-on wire surface:
//!
//! - [`Parameters`] — Simplex tunables (`parameters.go`).
//! - [`messages`] — the canoto [`QC`](messages::QC) (`qc.go`/`qc.canoto.go`) and
//!   the BLS [`BlsVerifier`](messages::BlsVerifier) / aggregation rules
//!   (`bls.go`), reusing [`ava_crypto::bls`].
//! - [`block`] — the [`Block`](block::Block) wrapper and its
//!   [`ProtocolMetadata`](block::ProtocolMetadata) header (`block.go`/
//!   `block.canoto.go`).
//! - [`canoto`] — hand-rolled canoto wire primitives (there is no canoto codegen
//!   crate in this workspace; the message types round-trip **byte-identical** to
//!   Go's generated `*.canoto.go`).
//!
//! The round-based BFT engine itself is a **feature-gated stub**
//! ([`engine::SimplexEngine`], `#[cfg(feature = "simplex")]`, off by default)
//! presenting the [`ava_engine`] `Engine`/`Handler` surface; the full BFT state
//! machine is deferred.

#![forbid(unsafe_code)]
// Dev-dependencies (assert_matches, tokio) are exercised only by the integration
// test crates under `tests/`, so the unit-test build of the library sees them as
// unused. Mirror the repo convention (cf. ava-vm test crates).
#![cfg_attr(test, allow(unused_crate_dependencies))]

pub mod block;
pub mod canoto;
pub mod error;
pub mod messages;
pub mod parameters;

#[cfg(feature = "simplex")]
pub mod engine;

pub use block::{Block, ProtocolMetadata};
pub use error::{Error, Result};
pub use messages::{BlockHeader, BlsVerifier, Finalization, QC, Vote, aggregate, quorum};
pub use parameters::{Parameters, ValidatorInfo};

#[cfg(feature = "simplex")]
pub use engine::SimplexEngine;
