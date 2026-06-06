// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.6 — `golden::message_frames`: per-op wire-byte vectors captured from the
//! Go node (`message/messages_test.go` path, deterministic `proto.Marshal`).
//!
//! For uncompressed ops we assert **byte-identical** `frame(out.bytes)`; for any
//! zstd op we would assert only cross-decodability (R4) — the committed vectors
//! here are all uncompressed, so byte-equality applies throughout.
//!
//! Provenance: extracted from `github.com/ava-labs/avalanchego` `proto/pb/p2p`
//! via a scratch `proto.MarshalOptions{Deterministic:true}` program built
//! against the `p2p` package (specs/02 §6.2). Frame = `len_be_u32 || proto_bytes`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    unused_crate_dependencies
)]

use std::net::{Ipv6Addr, SocketAddr};

use bytes::Bytes;
use pretty_assertions::assert_eq;
use serde::Deserialize;

use ava_message::builder::{Creator, OutboundMsgBuilder};
use ava_message::codec::{Compression, MsgBuilder, OutboundMessage};
use ava_message::frame::write_msg_len;
use ava_message::ops::Op;
use ava_message::proto::p2p;
use ava_types::id::Id;

#[derive(Debug, Deserialize)]
struct Vector {
    #[allow(dead_code)]
    input_fields: serde_json::Value,
    hex_frame: String,
}

/// Prepends the 4-byte big-endian length prefix to the proto bytes, producing
/// the on-wire frame.
fn frame(out: &OutboundMessage) -> Vec<u8> {
    let mut buf = bytes::BytesMut::new();
    write_msg_len(
        &mut buf,
        u32::try_from(out.bytes.len()).expect("len fits u32"),
    )
    .expect("cap");
    buf.extend_from_slice(&out.bytes);
    buf.to_vec()
}

fn load(name: &str) -> Vector {
    let path = format!(
        "{}/../../tests/vectors/message/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("parse {path}: {e}"))
}

fn id_of(byte: u8) -> Id {
    Id::from([byte; 32])
}

/// Builds the uncompressed `OutboundMessage` for a given vector name. The
/// handshake-class ops go through the `Creator`; the consensus/app ops build the
/// proto directly and marshal uncompressed (their builders are deferred).
fn build(name: &str) -> OutboundMessage {
    // Creator forced to uncompressed so get_peerlist/peer_list match the
    // uncompressed golden bytes (the live Creator defaults to zstd for those).
    let creator = Creator::with_compression(
        std::sync::Arc::new(MsgBuilder::default()),
        Compression::None,
    );
    let mb = MsgBuilder::default();

    match name {
        "handshake" => {
            let ip = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 9651);
            creator
                .handshake(
                    1337,
                    1_700_000_000,
                    ip,
                    "avalanchego",
                    1,
                    14,
                    2,
                    1_699_000_000,
                    1_700_000_000,
                    b"tls-sig",
                    b"bls-sig",
                    &[id_of(7)],
                    &[10, 20],
                    &[30],
                    &[0xAA; 4],
                    &[0xBB; 8],
                    true,
                )
                .expect("handshake")
        }
        "ping" => creator.ping(73).expect("ping"),
        "ping_zero" => creator.ping(0).expect("ping_zero"),
        "pong" => creator.pong().expect("pong"),
        "get_peerlist" => creator
            .get_peer_list(&[1, 2, 3], &[4, 5], true)
            .expect("get_peerlist"),
        "peerlist" => {
            let peer = p2p::ClaimedIpPort {
                x509_certificate: Bytes::from_static(b"cert"),
                ip_addr: Bytes::from(vec![0u8; 16]),
                ip_port: 10,
                timestamp: 1,
                signature: Bytes::from_static(&[0]),
                tx_id: Bytes::from(vec![0u8; 32]),
            };
            creator.peer_list(&[peer], true).expect("peerlist")
        }
        "get" => {
            let m = p2p::Message {
                message: Some(p2p::message::Message::Get(p2p::Get {
                    chain_id: Bytes::from(vec![0x11; 32]),
                    request_id: 5,
                    deadline: 1_000_000_000,
                    container_id: Bytes::from(vec![0x22; 32]),
                })),
            };
            mb.create_outbound(&m, Compression::None, false)
                .expect("get")
        }
        "chits" => {
            let m = p2p::Message {
                message: Some(p2p::message::Message::Chits(p2p::Chits {
                    chain_id: Bytes::from(vec![0x11; 32]),
                    request_id: 9,
                    preferred_id: Bytes::from(vec![0x33; 32]),
                    accepted_id: Bytes::from(vec![0x44; 32]),
                    preferred_id_at_height: Bytes::from(vec![0x55; 32]),
                    accepted_height: 42,
                })),
            };
            mb.create_outbound(&m, Compression::None, false)
                .expect("chits")
        }
        "app_request" => {
            let m = p2p::Message {
                message: Some(p2p::message::Message::AppRequest(p2p::AppRequest {
                    chain_id: Bytes::from(vec![0x11; 32]),
                    request_id: 7,
                    deadline: 1_000_000_000,
                    app_bytes: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
                })),
            };
            mb.create_outbound(&m, Compression::None, false)
                .expect("app_request")
        }
        other => panic!("unknown vector {other}"),
    }
}

/// Expected op for each vector (sanity-checks the builder produced the right
/// oneof variant before the byte compare).
fn expected_op(name: &str) -> Op {
    match name {
        "handshake" => Op::Handshake,
        "ping" | "ping_zero" => Op::Ping,
        "pong" => Op::Pong,
        "get_peerlist" => Op::GetPeerList,
        "peerlist" => Op::PeerList,
        "get" => Op::Get,
        "chits" => Op::Chits,
        "app_request" => Op::AppRequest,
        other => panic!("unknown vector {other}"),
    }
}

#[test]
fn message_frames() {
    const VECTORS: &[&str] = &[
        "handshake",
        "ping",
        "ping_zero",
        "pong",
        "get_peerlist",
        "peerlist",
        "get",
        "chits",
        "app_request",
    ];

    for &name in VECTORS {
        let vec = load(name);
        let out = build(name);
        assert_eq!(out.op, expected_op(name), "op mismatch for {name}");
        let got = hex::encode(frame(&out));
        assert_eq!(got, vec.hex_frame, "byte mismatch for golden vector {name}");

        // We must also decode the Go frame back to the same op (read path).
        let mb = MsgBuilder::default();
        let raw = hex::decode(&vec.hex_frame).expect("hex");
        // Strip the 4-byte length prefix before unmarshal.
        let (_m, _saved, op) = mb.unmarshal(&raw[4..]).expect("unmarshal go frame");
        assert_eq!(
            op,
            expected_op(name),
            "go-frame decode op mismatch for {name}"
        );
    }
}
