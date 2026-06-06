// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Build script: generates the `p2p` proto message types from
//! `proto/p2p/p2p.proto` into `OUT_DIR` via `tonic-build` (prost). `proto/p2p`
//! defines **no gRPC service** — it rides the raw framed TLS stream (specs/05) —
//! so client/server stubs are disabled; only the message structs + the
//! `Message.message` oneof enum are emitted.
//!
//! Generated code is **not committed** (specs/15 §5, 00 decision 8): `proto.rs`
//! reaches it through `include!(concat!(env!("OUT_DIR"), "/p2p.rs"))`.
//!
//! All proto `bytes`/`repeated bytes` fields map to `bytes::Bytes` (`bytes(".")`)
//! for zero-copy on the p2p read path (specs/15 §5, 00 §9).
//!
//! `expect`/panic is idiomatic in a build script (failure must abort the build
//! with a clear message), so the workspace `expect_used` deny is relaxed here.
#![allow(clippy::expect_used)]

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is always set for build scripts");
    // The proto tree is vendored inside this crate: <crate>/proto/p2p/p2p.proto.
    let proto_root = std::path::Path::new(&manifest_dir).join("proto");
    let proto_file = proto_root.join("p2p/p2p.proto");

    // Rebuild if the proto or its directory changes.
    println!("cargo:rerun-if-changed={}", proto_file.display());
    println!("cargo:rerun-if-changed={}", proto_root.display());

    tonic_build::configure()
        // p2p carries no gRPC service; emit messages only.
        .build_client(false)
        .build_server(false)
        // Map every proto `bytes`/`repeated bytes` field to `bytes::Bytes`
        // (specs/15 §5) for zero-copy on the hot p2p read path.
        .bytes(["."])
        .compile_protos(&[proto_file], &[proto_root])
        .expect("failed to compile proto/p2p/p2p.proto");
}
