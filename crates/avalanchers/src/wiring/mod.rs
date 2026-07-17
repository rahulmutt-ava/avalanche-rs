// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Node-assembly wiring for the `avalanchers` binary.
//!
//! Each submodule wires an already-built crate into a running node. M3.28 adds
//! [`chains`] (in-process chain-manager + a built-in no-op test-VM factory +
//! one Snowman chain through the full `create_snowman_chain` pipeline).

pub mod chains;
pub mod genesis_validator_state;
