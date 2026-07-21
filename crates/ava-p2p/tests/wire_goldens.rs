// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Recorded Go-oracle wire goldens (cchain-tx-gossip Task 15).
//!
//! Byte-for-byte proof that Rust's varint-prefixed gossip frames
//! (`network::protocol_prefix` + prost-encoded `pb::sdk` messages) match Go's
//! `network/p2p.PrefixMessage(network/p2p.ProtocolPrefix(...), proto.Marshal(...))`
//! exactly, for the three SDK gossip message types the C-Chain tx-gossip
//! wiring uses (`PushGossip`, `PullGossipRequest`, `PullGossipResponse`).
//!
//! The goldens were emitted by the live Go oracle
//! `tests/differential/go-oracle/p2p_sdk_wire_emitter_test.go` (dropped into
//! `avalanchego/network/p2p/` to run — see that file's header for the
//! re-freeze command) at avalanchego commit `5c4d318161d2c34a14a635632738b739704aef7b`
//! (`rpcchainvm=45`); provenance is also recorded in
//! `tests/vectors/p2p_sdk/MANIFEST.md`.
//!
//! Each case is checked in both directions:
//! - **encode**: Rust builds the identical frame from the same fixed inputs
//!   and byte-compares it against the committed golden.
//! - **decode**: Rust parses the golden's varint prefix
//!   ([`ava_p2p::network::parse_prefix`]) and prost-decodes the remaining
//!   payload, asserting the decoded fields equal the fixed inputs.

use bytes::Bytes;
use prost::Message;

use ava_p2p::network::{parse_prefix, protocol_prefix};
use ava_p2p::pb::sdk;

// These crates are only reachable transitively through `ava-p2p`'s own
// dependency graph in this test binary (not used directly here); silence
// `unused_crate_dependencies` rather than dropping them from `[dependencies]`,
// where the library crate genuinely needs them.
use async_trait as _;
use ava_types as _;
use ava_utils as _;
use ava_version as _;
use ava_vm as _;
use parking_lot as _;
use thiserror as _;
use tokio as _;
use tokio_util as _;
use tracing as _;

const PUSH_GOSSIP_FRAME: &[u8] =
    include_bytes!("../../../tests/vectors/p2p_sdk/push_gossip_frame.bin");
const PULL_GOSSIP_REQUEST: &[u8] =
    include_bytes!("../../../tests/vectors/p2p_sdk/pull_gossip_request.bin");
const PULL_GOSSIP_RESPONSE: &[u8] =
    include_bytes!("../../../tests/vectors/p2p_sdk/pull_gossip_response.bin");

/// The fixed 32-byte salt (`0x01..=0x20`) the Go emitter used for
/// `PullGossipRequest.salt`.
fn fixed_salt() -> Bytes {
    Bytes::from((1u8..=32).collect::<Vec<u8>>())
}

/// The fixed 8-byte filter (`0xF0..=0xF7`) the Go emitter used for
/// `PullGossipRequest.filter`.
fn fixed_filter() -> Bytes {
    Bytes::from((0xF0u8..=0xF7).collect::<Vec<u8>>())
}

#[test]
fn push_gossip_frame_matches_go_oracle() {
    let msg = sdk::PushGossip {
        gossip: vec![
            Bytes::from_static(&[0xDE, 0xAD]),
            Bytes::from_static(&[0xBE, 0xEF]),
        ],
    };
    let mut frame = protocol_prefix(0);
    frame.extend_from_slice(&msg.encode_to_vec());

    assert_eq!(
        frame, PUSH_GOSSIP_FRAME,
        "Rust-built PushGossip frame must byte-match the Go oracle golden"
    );

    let (handler_id, payload) =
        parse_prefix(PUSH_GOSSIP_FRAME).expect("parse_prefix(push_gossip_frame.bin)");
    assert_eq!(handler_id, 0, "push_gossip_frame.bin handler-id prefix");
    let decoded =
        sdk::PushGossip::decode(payload).expect("sdk::PushGossip::decode(push_gossip_frame.bin)");
    assert_eq!(
        decoded, msg,
        "decoded PushGossip must equal the fixed input"
    );
}

#[test]
fn pull_gossip_request_matches_go_oracle() {
    let msg = sdk::PullGossipRequest {
        salt: fixed_salt(),
        filter: fixed_filter(),
    };
    let mut frame = protocol_prefix(0);
    frame.extend_from_slice(&msg.encode_to_vec());

    assert_eq!(
        frame, PULL_GOSSIP_REQUEST,
        "Rust-built PullGossipRequest frame must byte-match the Go oracle golden"
    );

    let (handler_id, payload) =
        parse_prefix(PULL_GOSSIP_REQUEST).expect("parse_prefix(pull_gossip_request.bin)");
    assert_eq!(handler_id, 0, "pull_gossip_request.bin handler-id prefix");
    let decoded = sdk::PullGossipRequest::decode(payload)
        .expect("sdk::PullGossipRequest::decode(pull_gossip_request.bin)");
    assert_eq!(
        decoded, msg,
        "decoded PullGossipRequest must equal the fixed input"
    );
}

#[test]
fn pull_gossip_response_matches_go_oracle() {
    let msg = sdk::PullGossipResponse {
        gossip: vec![Bytes::from_static(&[0xCA, 0xFE])],
    };
    let mut frame = protocol_prefix(0);
    frame.extend_from_slice(&msg.encode_to_vec());

    assert_eq!(
        frame, PULL_GOSSIP_RESPONSE,
        "Rust-built PullGossipResponse frame must byte-match the Go oracle golden"
    );

    let (handler_id, payload) =
        parse_prefix(PULL_GOSSIP_RESPONSE).expect("parse_prefix(pull_gossip_response.bin)");
    assert_eq!(handler_id, 0, "pull_gossip_response.bin handler-id prefix");
    let decoded = sdk::PullGossipResponse::decode(payload)
        .expect("sdk::PullGossipResponse::decode(pull_gossip_response.bin)");
    assert_eq!(
        decoded, msg,
        "decoded PullGossipResponse must equal the fixed input"
    );
}
