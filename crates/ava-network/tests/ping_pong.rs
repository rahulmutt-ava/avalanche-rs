// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.16 — ping/pong + uptime tracking + `should_disconnect` compat re-check
//! (`specs/05` §1.5/§3.2, `specs/26` §3.1).

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

/// Drive a peer through the full handshake, leaving it `finished_handshake`.
async fn complete_handshake(
    h: &mut PeerHarness,
) -> (tokio::io::DuplexStream, ava_network::peer::handle::PeerHandle) {
    let (mut remote, peer) = h.spawn();
    // Peer's Handshake.
    let _ = read_one_frame(&mut remote).await.expect("peer handshake");
    // Our Handshake → peer replies PeerList.
    let hs = h.build_handshake(HandshakeOverrides::default());
    write_one_frame(&mut remote, &hs).await.expect("send hs");
    let _ = read_one_frame(&mut remote).await.expect("peerlist reply");
    // Our PeerList → finishes the handshake.
    let pl = h.build_peer_list();
    write_one_frame(&mut remote, &pl).await.expect("send pl");
    tokio::time::timeout(Duration::from_secs(5), peer.finished_handshake())
        .await
        .expect("handshake finishes");
    (remote, peer)
}

/// The net-task tick sends a `Ping{uptime}`; we reply `Pong`; the peer clears
/// its outstanding-ping marker (records the RTT).
#[tokio::test(start_paused = true)]
async fn ping_carries_uptime_and_pong_records_rtt() {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = complete_handshake(&mut h).await;

    // Advance virtual time past the ping interval to trigger a tick.
    tokio::time::advance(Duration::from_millis(23_000)).await;

    // The peer sends a Ping.
    let frame = tokio::time::timeout(Duration::from_secs(5), read_one_frame(&mut remote))
        .await
        .expect("ping within timeout")
        .expect("read ping");
    let (msg, _s, op) = ava_message::codec::MsgBuilder::default()
        .unmarshal(&frame)
        .expect("decode ping");
    assert_eq!(op, Op::Ping);
    if let Some(ava_message::proto::p2p::message::Message::Ping(p)) = msg.message {
        assert!(p.uptime <= 100);
    } else {
        panic!("expected Ping");
    }

    // Reply with a Pong — must not close the connection (a Ping was outstanding).
    let pong = h.build_pong();
    write_one_frame(&mut remote, &pong).await.expect("send pong");

    // Give the read task a moment; the peer stays open.
    tokio::task::yield_now().await;
    assert!(!peer.closed_now(), "solicited pong keeps the connection open");

    peer.close();
}

/// A `Ping{uptime = 101}` is rejected: the connection closes.
#[tokio::test]
async fn ping_uptime_over_100_closes() {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();
    let _ = read_one_frame(&mut remote).await.expect("peer handshake");

    let ping = h.build_ping(101);
    let _ = write_one_frame(&mut remote, &ping).await;

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("uptime > 100 closes");
}

/// An unsolicited `Pong` (no outstanding `Ping`) closes the connection.
#[tokio::test]
async fn unsolicited_pong_closes() {
    let mut h = PeerHarness::new();
    let (mut remote, peer) = h.spawn();
    let _ = read_one_frame(&mut remote).await.expect("peer handshake");

    let pong = h.build_pong();
    let _ = write_one_frame(&mut remote, &pong).await;

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("unsolicited pong closes");
}

/// A peer compatible under the pre-upgrade floor is dropped on the next tick
/// once the (mock) clock crosses `upgrade_time` (`specs/26` §3.1).
#[tokio::test(start_paused = true)]
async fn should_disconnect_on_clock_crossing_upgrade() {
    // upgrade_time = epoch + 1_700_000_100; the peer reports 1.13.5 which is
    // >= the pre-upgrade floor (1.13.0) but < the post-upgrade floor (1.14.0).
    let upgrade_time =
        std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_100);
    let mut h = PeerHarness::new()
        .with_upgrade_time(upgrade_time)
        .with_floors(
            Application::new("avalanchego", 1, 14, 0),
            Application::new("avalanchego", 1, 13, 0),
        );
    // Clock starts before the upgrade (1_700_000_000), so 1.13.5 is compatible.
    h.clock().set(1_700_000_000);

    let (mut remote, peer) = h.spawn();
    let _ = read_one_frame(&mut remote).await.expect("peer handshake");
    let hs = h.build_handshake(HandshakeOverrides {
        version: Some(Application::new("avalanchego", 1, 13, 5)),
        ..Default::default()
    });
    write_one_frame(&mut remote, &hs).await.expect("send hs");
    let _ = read_one_frame(&mut remote).await.expect("peerlist reply");
    let pl = h.build_peer_list();
    write_one_frame(&mut remote, &pl).await.expect("send pl");
    tokio::time::timeout(Duration::from_secs(5), peer.finished_handshake())
        .await
        .expect("handshake finishes pre-upgrade");

    // Cross the upgrade boundary on the mock clock; the next tick must drop.
    h.clock().set(1_700_000_200);
    tokio::time::advance(Duration::from_millis(23_000)).await;

    tokio::time::timeout(Duration::from_secs(5), peer.closed())
        .await
        .expect("peer dropped after clock crosses upgrade_time");
}
