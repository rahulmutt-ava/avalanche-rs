// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Build script: generates the `sync` proto request/response types (+ a thin
//! gRPC client/server stub) from `proto/sync/sync.proto` into `OUT_DIR` via
//! `tonic-build`.
//!
//! Generated code is **not committed** (specs/01 §8.1, 00 decision 8): consumers
//! reach it through `tonic::include_proto!("sync")`. Codegen runs only when the
//! `sync` feature is active, so non-sync builds never need `protoc`.
//!
//! All proto `bytes` fields map to `bytes::Bytes` (`bytes(".")`) for zero-copy on
//! the hot key/value/proof paths (specs/15 §5).
//!
//! `expect`/panic is idiomatic in a build script (failure must abort the build
//! with a clear message), so the workspace `expect_used` deny is relaxed here.
#![allow(clippy::expect_used)]

fn main() {
    // Only generate when the consuming build enabled the `sync` feature. Cargo
    // exposes each active feature as `CARGO_FEATURE_<NAME>` to the build script.
    if std::env::var_os("CARGO_FEATURE_SYNC").is_none() {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set for build scripts");
    // The shared proto tree lives at the workspace root: <crate>/../../proto.
    let proto_root = std::path::Path::new(&manifest_dir).join("../../proto");
    let proto_file = proto_root.join("sync/sync.proto");

    // Rebuild if the proto or its directory changes.
    println!("cargo:rerun-if-changed={}", proto_file.display());
    println!("cargo:rerun-if-changed={}", proto_root.display());

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        // Map every proto `bytes` field to `bytes::Bytes` (specs/15 §5).
        .bytes(["."])
        .compile_protos(&[proto_file], &[proto_root])
        .expect("failed to compile proto/sync/sync.proto");
}
