// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.5 — the `Creator` / `OutboundMsgBuilder` API (specs/05 §2.4).

use std::net::{Ipv6Addr, SocketAddr};

use ava_types::id::Id;

use ava_message::builder::{Creator, OutboundMsgBuilder};
use ava_message::codec::MsgBuilder;
use ava_message::ops::Op;
use ava_message::proto::p2p;

fn creator() -> Creator {
    Creator::new(MsgBuilder::default())
}

fn unwrap_inner(out_bytes: &[u8]) -> p2p::message::Message {
    let mb = MsgBuilder::default();
    let (m, _saved, _op) = mb.unmarshal(out_bytes).expect("unmarshal");
    m.message.expect("oneof set")
}

#[test]
fn build_handshake_sets_fields() {
    let c = creator();
    let ip = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 9651);
    let subnet = Id::from([7u8; 32]);

    let out = c
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
            &[subnet],
            &[10, 20],
            &[30],
            &[0xAA; 4],
            &[0xBB; 8],
            true,
        )
        .expect("handshake");

    assert_eq!(out.op, Op::Handshake);
    // Handshake replies bypass throttling.
    assert!(out.bypass_throttling);

    match unwrap_inner(&out.bytes) {
        p2p::message::Message::Handshake(h) => {
            assert_eq!(h.network_id, 1337);
            assert_eq!(h.my_time, 1_700_000_000);
            // ip_addr is the 16-byte As16 form.
            assert_eq!(h.ip_addr.len(), 16);
            assert_eq!(&h.ip_addr[..], &[0u8; 16]);
            assert_eq!(h.ip_port, 9651);
            let client = h.client.expect("client");
            assert_eq!(client.name, "avalanchego");
            assert_eq!((client.major, client.minor, client.patch), (1, 14, 2));
            assert_eq!(h.tracked_subnets.len(), 1);
            assert_eq!(&h.tracked_subnets[0][..], &[7u8; 32]);
            assert_eq!(h.supported_acps, vec![10, 20]);
            assert_eq!(h.objected_acps, vec![30]);
            let kp = h.known_peers.expect("known_peers");
            assert_eq!(&kp.filter[..], &[0xAA; 4]);
            assert_eq!(&kp.salt[..], &[0xBB; 8]);
            assert_eq!(&h.ip_node_id_sig[..], b"tls-sig");
            assert_eq!(&h.ip_bls_sig[..], b"bls-sig");
            assert!(h.all_subnets);
        }
        other => panic!("expected Handshake, got {other:?}"),
    }
}

#[test]
fn build_ping_sets_uptime() {
    let c = creator();
    let out = c.ping(73).expect("ping");
    assert_eq!(out.op, Op::Ping);
    assert!(!out.bypass_throttling);
    match unwrap_inner(&out.bytes) {
        p2p::message::Message::Ping(p) => assert_eq!(p.uptime, 73),
        other => panic!("expected Ping, got {other:?}"),
    }
}

#[test]
fn build_pong() {
    let c = creator();
    let out = c.pong().expect("pong");
    assert_eq!(out.op, Op::Pong);
    assert!(matches!(
        unwrap_inner(&out.bytes),
        p2p::message::Message::Pong(_)
    ));
}

#[test]
fn build_get_peer_list() {
    let c = creator();
    let out = c
        .get_peer_list(&[1, 2, 3], &[4, 5], true)
        .expect("get_peer_list");
    assert_eq!(out.op, Op::GetPeerList);
    assert!(!out.bypass_throttling);
    match unwrap_inner(&out.bytes) {
        p2p::message::Message::GetPeerList(g) => {
            let kp = g.known_peers.expect("known_peers");
            assert_eq!(&kp.filter[..], &[1, 2, 3]);
            assert_eq!(&kp.salt[..], &[4, 5]);
            assert!(g.all_subnets);
        }
        other => panic!("expected GetPeerList, got {other:?}"),
    }
}

#[test]
fn build_peer_list_bypass_throttling_true() {
    let c = creator();
    let peer = p2p::ClaimedIpPort {
        x509_certificate: bytes::Bytes::from_static(b"cert"),
        ip_addr: bytes::Bytes::from(vec![0u8; 16]),
        ip_port: 10,
        timestamp: 1,
        signature: bytes::Bytes::from_static(&[0]),
        tx_id: bytes::Bytes::from(vec![0u8; 32]),
    };
    let out = c.peer_list(&[peer], true).expect("peer_list");
    assert_eq!(out.op, Op::PeerList);
    assert!(out.bypass_throttling);
    match unwrap_inner(&out.bytes) {
        p2p::message::Message::PeerList(pl) => {
            assert_eq!(pl.claimed_ip_ports.len(), 1);
            assert_eq!(pl.claimed_ip_ports[0].ip_port, 10);
        }
        other => panic!("expected PeerList, got {other:?}"),
    }
}
