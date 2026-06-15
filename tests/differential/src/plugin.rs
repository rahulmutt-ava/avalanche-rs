// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! rpcchainvm plugin-interop harness helpers (specs/07 §5.1, plan/M9 §M9.3/§M9.12).
//!
//! Helpers to build a Rust test-VM plugin binary, launch it under a live Go
//! `avalanchego` host (and, in the reverse direction, host a Go plugin under a
//! Rust node), and assert the v45 reverse-dial handshake completes. The live
//! arms are gated behind the `live` Cargo feature + `#[ignore]` (they need an
//! external Go binary at `$AVALANCHEGO_PATH`); see the integration tests in
//! `tests/plugin_rust_in_go.rs`.
//!
//! Filled in by plan/M9 task M9.3.
