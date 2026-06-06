// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Generated protobuf types for the inter-node p2p wire schema
//! (`proto/p2p/p2p.proto`, specs/15 §3.1).
//!
//! The module is produced by `build.rs` (prost via `tonic-build`) into
//! `OUT_DIR` and pulled in here with `include!`; it is **not committed**
//! (specs/15 §5). `prost` and Go's protobuf emit identical proto3 wire bytes
//! (fields in ascending tag order, zero/empty scalars elided), which is what
//! makes the framing byte-exact (specs/05 §1.1 note).

/// The `p2p` package: `Message` (root oneof wrapper), the `message::Message`
/// oneof enum, and every sub-message (`Ping`, `Handshake`, `PeerList`,
/// `ClaimedIpPort`, `Get`, `Put`, `Chits`, `AppRequest`, `Simplex`, …).
pub mod p2p {
    include!(concat!(env!("OUT_DIR"), "/p2p.rs"));
}
