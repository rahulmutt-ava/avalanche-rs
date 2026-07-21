// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Recorded Go-oracle wire-ENVELOPE goldens (cchain-tx-gossip Task 16 live
//! debugging — Probe 1).
//!
//! Byte-for-byte proof that Rust's outbound `p2p.Message` envelope bytes
//! ([`MsgBuilder::create_outbound`] with [`Compression::None`], the exact
//! call shape `crates/ava-engine/src/networking/sender.rs`'s `dispatch`/
//! `gossip` use) match Go's `message.Creator.AppGossip`/`AppRequest`
//! (`OutboundMessage.Bytes`, i.e. `messages.go`'s `createOutbound`/`marshal`
//! with `compression.TypeNone`) exactly, for a fixed chain id and payload.
//!
//! This isolates whether the established live fact — the Rust node's
//! C-Chain tx-gossip `AppGossip` is never observed by any Go peer, while its
//! `AppRequest`/`AppResponse` interop with Go fine — could be explained by a
//! Rust-side encoding bug specific to the `AppGossip` variant. `AppRequest`
//! is the known-good baseline: if it matches and `AppGossip` doesn't, the
//! delta IS the bug; if both match, envelope encoding is exonerated and the
//! fault lies elsewhere (peer-level parsing/logging, or something specific
//! to the live topology).
//!
//! The goldens were emitted by the live Go oracle
//! `tests/differential/go-oracle/message_envelope_wire_emitter_test.go`
//! (dropped into `avalanchego/message/` to run — see that file's header for
//! the re-freeze command) against the same `~/avalanchego` checkout pinned
//! by `tests/vectors/p2p_sdk/MANIFEST.md` (cchain-tx-gossip Task 15).

use bytes::Bytes;

use ava_message::codec::{Compression, MsgBuilder};
use ava_message::proto::p2p;

// This crate is only reachable transitively through `ava-message`'s own
// dependency graph in this test binary (not used directly here); silence
// `unused_crate_dependencies` rather than dropping it from `[dependencies]`,
// where the library crate genuinely needs it.
use prost as _;

const APP_GOSSIP_ENVELOPE: &[u8] =
    include_bytes!("../../../tests/vectors/message_envelope/app_gossip_envelope.bin");
const APP_REQUEST_ENVELOPE: &[u8] =
    include_bytes!("../../../tests/vectors/message_envelope/app_request_envelope.bin");

/// The fixed 32-byte chain id (`0x01..=0x20`) the Go emitter used — matches
/// the Task 15 `p2p_sdk` emitter's salt convention.
fn fixed_chain_id() -> Bytes {
    Bytes::from((1u8..=32).collect::<Vec<u8>>())
}

/// The T15 `push_gossip_frame.bin` bytes: the varint-handler-id-prefixed
/// `PushGossip` SDK frame — exactly what `ava_p2p::client::Client::app_gossip`
/// hands `AppSender::send_app_gossip` as `app_bytes` in production. Used
/// verbatim as the `app_bytes` payload for both cases here (its internal
/// shape is opaque to the envelope layer either way).
fn fixed_app_bytes() -> Bytes {
    Bytes::from_static(include_bytes!(
        "../../../tests/vectors/p2p_sdk/push_gossip_frame.bin"
    ))
}

#[test]
fn app_gossip_envelope_matches_go_oracle() {
    let msg = p2p::Message {
        message: Some(p2p::message::Message::AppGossip(p2p::AppGossip {
            chain_id: fixed_chain_id(),
            app_bytes: fixed_app_bytes(),
        })),
    };
    let mb = MsgBuilder::default();
    let out = mb
        .create_outbound(&msg, Compression::None, false)
        .expect("MsgBuilder::create_outbound(AppGossip)");
    assert_eq!(
        out.bytes.as_ref(),
        APP_GOSSIP_ENVELOPE,
        "Rust-built AppGossip envelope must byte-match the Go oracle golden"
    );
}

#[test]
fn app_request_envelope_matches_go_oracle() {
    let msg = p2p::Message {
        message: Some(p2p::message::Message::AppRequest(p2p::AppRequest {
            chain_id: fixed_chain_id(),
            request_id: 1,
            deadline: 1_000_000_000,
            app_bytes: fixed_app_bytes(),
        })),
    };
    let mb = MsgBuilder::default();
    let out = mb
        .create_outbound(&msg, Compression::None, false)
        .expect("MsgBuilder::create_outbound(AppRequest)");
    assert_eq!(
        out.bytes.as_ref(),
        APP_REQUEST_ENVELOPE,
        "Rust-built AppRequest envelope must byte-match the Go oracle golden (known-good \
         baseline: production AppRequest/AppResponse already interop with real Go peers live)"
    );
}
