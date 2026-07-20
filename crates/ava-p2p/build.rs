// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Build script: generates the `sdk` proto message types from
//! `proto/sdk/sdk.proto` into `OUT_DIR` via `prost-build`. `proto/sdk` defines
//! **no gRPC service** (it is carried as opaque `AppRequest`/`AppGossip`
//! payload bytes over the raw p2p stream), so only message structs are
//! emitted — no client/server stubs (compare `crates/ava-message/build.rs`,
//! which uses `tonic-build` with services disabled for the same reason).
//!
//! Generated code is **not committed** (specs/15 §5, 00 decision 8): `pb.rs`
//! reaches it through `include!(concat!(env!("OUT_DIR"), "/sdk.rs"))`.
//!
//! All proto `bytes`/`repeated bytes` fields map to `bytes::Bytes`
//! (`.bytes(["."])`) for zero-copy on the gossip read path (specs/15 §5, 00 §9),
//! matching `ava-message`'s convention.
//!
//! `expect`/panic is idiomatic in a build script (failure must abort the build
//! with a clear message), so the workspace `expect_used` deny is relaxed here.
#![allow(clippy::expect_used)]

fn main() {
    prost_build::Config::new()
        .bytes(["."])
        .compile_protos(&["proto/sdk/sdk.proto"], &["proto"])
        .expect("failed to compile proto/sdk/sdk.proto");
    println!("cargo:rerun-if-changed=proto/sdk/sdk.proto");
}
