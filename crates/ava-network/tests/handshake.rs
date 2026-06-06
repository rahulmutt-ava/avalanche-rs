// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.15 — `handle_handshake`: all `specs/05` §1.4 disconnect reasons, the
//! `PeerList` reply, and the handshake-completion → `ExternalHandler::connected`
//! handoff (`specs/26` §3).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::time::Duration;

use ava_message::ops::Op;
use ava_network::peer::testutil::{
    read_one_frame, write_one_frame, HandshakeOverrides, PeerHarness,
};
use ava_version::Application;

/// Two in-process peers complete the handshake: each replies `PeerList` to the
/// other's `Handshake`, `finished_handshake` latches, and `connected` fires
/// once.
#[tokio::test]
async fn handshake_then_peerlist_completes() {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();

    // The peer's first frame is its Handshake.
    let frame = read_one_frame(&mut remote).await.expect("peer handshake");
    let (_m, _s, op) = ava_message::codec::MsgBuilder::default()
        .unmarshal(&frame)
        .expect("decode");
    assert_eq!(op, Op::Handshake);

    // Send our (valid) Handshake to the peer.
    let hs = h.build_handshake(HandshakeOverrides::default());
    write_one_frame(&mut remote, &hs).await.expect("send handshake");

    // The peer must reply with a PeerList.
    let reply = read_one_frame(&mut remote).await.expect("peer reply");
    let (_m, _s, op) = ava_message::codec::MsgBuilder::default()
        .unmarshal(&reply)
        .expect("decode reply");
    assert_eq!(op, Op::PeerList, "peer replies PeerList to our Handshake");

    // Now send our PeerList to finish the handshake.
    let pl = h.build_peer_list();
    write_one_frame(&mut remote, &pl).await.expect("send peerlist");

    tokio::time::timeout(Duration::from_secs(5), peer.finished_handshake())
        .await
        .expect("handshake finishes");

    // `connected` fired exactly once.
    assert_eq!(h.router().connected_count(), 1);

    peer.close();
}

/// Each disconnect case: the peer closes before `connected`.
#[tokio::test]
async fn disconnect_reasons_close_the_connection() {
    // (name, overrides, builder-customization)
    let cases: Vec<(&str, HandshakeOverrides)> = vec![
        (
            "wrong network id",
            HandshakeOverrides {
                network_id: Some(999),
                ..Default::default()
            },
        ),
        (
            "clock skew > 60s",
            HandshakeOverrides {
                my_time: Some(1_700_000_000 + 120),
                ..Default::default()
            },
        ),
        (
            "incompatible version",
            HandshakeOverrides {
                version: Some(Application::new("avalanchego", 1, 12, 0)),
                ..Default::default()
            },
        ),
        (
            "too many tracked subnets",
            HandshakeOverrides {
                num_tracked_subnets: Some(17),
                ..Default::default()
            },
        ),
        (
            "supported/objected overlap",
            HandshakeOverrides {
                supported_acps: Some(vec![1, 2, 3]),
                objected_acps: Some(vec![3, 4]),
                ..Default::default()
            },
        ),
        (
            "zero port",
            HandshakeOverrides {
                port: Some(0),
                ..Default::default()
            },
        ),
        (
            "bad ip signature",
            HandshakeOverrides {
                corrupt_ip_sig: true,
                ..Default::default()
            },
        ),
        (
            "bloom salt too long",
            HandshakeOverrides {
                bloom_salt_len: Some(33),
                ..Default::default()
            },
        ),
    ];

    for (name, overrides) in cases {
        let mut h = PeerHarness::new();
        let (mut remote, peer) = h.spawn();

        // Drain the peer's own handshake.
        let _ = read_one_frame(&mut remote)
            .await
            .unwrap_or_else(|_| panic!("[{name}] peer handshake"));

        let hs = h.build_handshake(overrides);
        let _ = write_one_frame(&mut remote, &hs).await;

        tokio::time::timeout(Duration::from_secs(5), peer.closed())
            .await
            .unwrap_or_else(|_| panic!("[{name}] peer must close"));

        assert_eq!(
            h.router().connected_count(),
            0,
            "[{name}] connected must NOT fire"
        );
    }
}

/// A duplicate Handshake on the same connection closes it.
#[tokio::test]
async fn duplicate_handshake_closes() {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();

    let _ = read_one_frame(&mut remote).await.expect("peer handshake");

    let hs1 = h.build_handshake(HandshakeOverrides::default());
    write_one_frame(&mut remote, &hs1).await.expect("hs1");
    // Drain the PeerList reply.
    let _ = read_one_frame(&mut remote).await.expect("peerlist reply");

    let hs2 = h.build_handshake(HandshakeOverrides::default());
    let _ = write_one_frame(&mut remote, &hs2).await;

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("duplicate handshake closes");
}
