// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avalanchers` node library — node-assembly wiring shared by the binary
//! and exercised by the integration tests.
//!
//! The binary (`src/main.rs`) is a thin entrypoint over this crate; the wiring
//! modules under [`wiring`] assemble already-built crates into a running node as
//! they land in successive milestones.

#![forbid(unsafe_code)]

pub mod wiring;
