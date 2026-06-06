// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-message` — the p2p wire framing & message codec.
//!
//! A from-scratch, **byte-exact** port of avalanchego's `message` package and
//! the `network/peer` framing helpers (specs/05 "Networking & P2P", specs/15
//! §3.1 "p2p oneof tags"). The inter-node p2p protocol MUST be indistinguishable
//! on the wire from a Go node: identical length-framing, identical proto3
//! encoding, identical op semantics.
//!
//! Contents:
//! - [`proto::p2p`] — the generated protobuf types (root `Message` oneof + all
//!   sub-messages), produced by `build.rs` and pulled in via `include!`.

#![forbid(unsafe_code)]

pub mod builder;
pub mod codec;
pub mod error;
pub mod frame;
pub mod ops;
pub mod proto;

pub use error::{Error, Result};
