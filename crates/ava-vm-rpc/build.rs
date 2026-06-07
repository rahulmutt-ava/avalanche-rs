// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Build script: generates the rpcchainvm gRPC client/server + prost types from
//! the **shared** `proto/` tree via `tonic-build`.
//!
//! Generated code is **not committed** (specs/01 §8.1, 00 decision 8): consumers
//! reach it through `tonic::include_proto!("<pkg>")`.
//!
//! Compiled packages:
//! * `vm` (`proto/vm/vm.proto`) — the `VM` service (07 §5.4). Imports the
//!   vendored `io/prometheus/client/metrics.proto` for the `Gather` RPC.
//! * `vm.runtime` (`proto/vm/runtime/runtime.proto`) — the handshake `Runtime`
//!   service (07 §5.1).
//! * `appsender`, `sharedmemory`, `validatorstate`, `warp`, `aliasreader` — the
//!   proxied callback services (07 §5.4, M3.25).
//!
//! All proto `bytes` fields map to `bytes::Bytes` (`bytes(".")`) for zero-copy on
//! the hot block/key/value paths (specs/15 §5).
//!
//! `expect`/panic is idiomatic in a build script (failure must abort the build
//! with a clear message), so the workspace `expect_used` deny is relaxed here.
#![allow(clippy::expect_used)]

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set for build scripts");
    // The shared proto tree lives at the workspace root: <crate>/../../proto.
    let proto_root = std::path::Path::new(&manifest_dir).join("../../proto");

    let protos = [
        "vm/vm.proto",
        "vm/runtime/runtime.proto",
        "appsender/appsender.proto",
        "sharedmemory/sharedmemory.proto",
        "validatorstate/validator_state.proto",
        "warp/message.proto",
        "aliasreader/aliasreader.proto",
    ];

    // Rebuild if any proto (or its directory) changes.
    println!("cargo:rerun-if-changed={}", proto_root.display());
    let proto_paths: Vec<_> = protos.iter().map(|p| proto_root.join(p)).collect();
    for p in &proto_paths {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        // Map every proto `bytes` field to `bytes::Bytes` (specs/15 §5).
        .bytes(["."])
        .compile_protos(&proto_paths, &[proto_root])
        .expect("failed to compile rpcchainvm protos");
}
