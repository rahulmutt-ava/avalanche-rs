// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generated `proposervm` Connect message types
//! (`proto/proposervm/service.proto`, copied verbatim from Go
//! `connectproto/proposervm/service.proto`).
//!
//! prost codegen runs in `build.rs` into `OUT_DIR` and is **not committed**
//! (specs/01 §8.1, 00 decision 8). Only message types are generated — the
//! Connect-unary transport over them is hand-rolled in [`crate::connect`].

// Generated code is exempt from the crate's documentation / pedantic lints.
#[allow(
    missing_docs,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::derive_partial_eq_without_eq
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/proposervm.rs"));
}

pub use generated::{
    GetCurrentEpochReply, GetCurrentEpochRequest, GetProposedHeightReply, GetProposedHeightRequest,
};
