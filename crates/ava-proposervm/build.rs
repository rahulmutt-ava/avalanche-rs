// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Build script: generates the `proposervm` Connect message types from the
//! **shared** `proto/` tree (`proto/proposervm/service.proto`, copied verbatim
//! from Go `connectproto/proposervm/service.proto`).
//!
//! Generated code is **not committed** (specs/01 §8.1, 00 decision 8): the
//! crate reaches it through `include!(concat!(env!("OUT_DIR"),
//! "/proposervm.rs"))` (see `src/pb.rs`).
//!
//! Only the prost **message types** are generated (`build_client(false)` /
//! `build_server(false)`): the Connect-unary transport is a hand-rolled
//! handler over the buffered `ava_vm::VmHttpService` seam (M8.22), so no
//! tonic client/server stubs are needed.
//!
//! `expect`/panic is idiomatic in a build script (failure must abort the build
//! with a clear message), so the workspace `expect_used` deny is relaxed here.
#![allow(clippy::expect_used)]

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set for build scripts");
    // The shared proto tree lives at the workspace root: <crate>/../../proto.
    let proto_root = std::path::Path::new(&manifest_dir).join("../../proto");
    let proto = proto_root.join("proposervm/service.proto");

    println!("cargo:rerun-if-changed={}", proto.display());

    tonic_build::configure()
        .build_client(false)
        .build_server(false)
        .compile_protos(&[proto], &[proto_root])
        .expect("failed to compile proposervm proto");
}
