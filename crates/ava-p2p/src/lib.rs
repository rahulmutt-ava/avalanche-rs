// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-p2p` — port of Go `network/p2p` + `network/p2p/gossip`.
//!
//! This crate is under active development as part of the C-Chain tx gossip
//! effort; this scaffold lands the generated `proto/sdk` messages
//! ([`pb::sdk`]) and the crate's error model ([`error`]). Handler/mux/client/
//! gossip modules land in later tasks.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod pb;

pub use error::{Error, Result};
