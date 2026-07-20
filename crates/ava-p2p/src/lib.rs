// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-p2p` — port of Go `network/p2p` + `network/p2p/gossip`.
//!
//! This crate is under active development as part of the C-Chain tx gossip
//! effort. So far it has the generated `proto/sdk` messages ([`pb::sdk`]),
//! the crate's error model ([`error`]), the per-protocol [`handler::Handler`]
//! trait, the varint-prefixed protocol mux ([`network::P2pNetwork`]),
//! request/response correlation ([`client::Client`]), and the gossip
//! framework traits + bloom-filtered id set ([`gossip`]).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod error;
pub mod gossip;
pub mod handler;
pub mod network;
pub mod pb;

pub use client::Client;
pub use error::{Error, Result};
