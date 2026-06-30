// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.18 — dialer + accept loop + `Network::dispatch` + runTimers + graceful
//! `start_close` (`specs/05` §3.1/§3.4, `specs/17` §2 #1/#2/#3/#4, §4.3).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::sync::Arc;
use std::time::Duration;

use ava_network::network::Network;
use ava_network::network::testutil::TestNetwork;

/// Two networks on loopback connect: A manually-tracks B, dials it, and both
/// reach `connected` (B appears in A's connected set).
#[tokio::test]
async fn two_networks_connect_locally() {
    let a = TestNetwork::start().await;
    let b = TestNetwork::start().await;

    // A learns B's listen address and pins it.
    a.network().manually_track(b.node_id(), b.listen_addr());

    // Dispatch both networks.
    let a_dispatch = {
        let net = Arc::clone(a.network());
        tokio::spawn(async move { net.dispatch().await })
    };
    let b_dispatch = {
        let net = Arc::clone(b.network());
        tokio::spawn(async move { net.dispatch().await })
    };

    // Wait until A reports B connected (or time out).
    let connected = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if a.network().connected_peers().contains(&b.node_id()) {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(connected, "A should see B as connected after dialing");

    a.network().start_close();
    b.network().start_close();
    let _ = tokio::time::timeout(Duration::from_secs(10), a_dispatch).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), b_dispatch).await;
}

/// `start_close` cancels the network token, drains every peer task, and
/// `dispatch` returns (`specs/17` §4.3).
#[tokio::test]
async fn start_close_drains_all_tasks() {
    let a = TestNetwork::start().await;
    let net = Arc::clone(a.network());
    let dispatch = tokio::spawn(async move { net.dispatch().await });

    // Let the accept loop / dialer / timers come up.
    tokio::time::sleep(Duration::from_millis(100)).await;

    a.network().start_close();

    let res = tokio::time::timeout(Duration::from_secs(10), dispatch)
        .await
        .expect("dispatch returns after start_close");
    assert!(res.is_ok(), "dispatch task joined cleanly");
}

/// Both networks track each other and dispatch, so each side gets a racing
/// inbound + outbound for the same peer. After the fix, each side's router
/// records the other peer `connected` EXACTLY ONCE (at-most-once delivery to an
/// ExternalHandler — M9.15 inbound dedup).
#[tokio::test]
async fn mutual_dial_connects_each_peer_exactly_once() {
    let a = TestNetwork::start().await;
    let b = TestNetwork::start().await;

    a.network().manually_track(b.node_id(), b.listen_addr());
    b.network().manually_track(a.node_id(), a.listen_addr());

    let a_dispatch = {
        let net = Arc::clone(a.network());
        tokio::spawn(async move { net.dispatch().await })
    };
    let b_dispatch = {
        let net = Arc::clone(b.network());
        tokio::spawn(async move { net.dispatch().await })
    };

    // Wait until both sides see each other connected (or time out).
    let both = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if a.network().connected_peers().contains(&b.node_id())
                && b.network().connected_peers().contains(&a.node_id())
            {
                break true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(both, "both networks should connect to each other");

    // Let any racing duplicate connection settle.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let a_count = a
        .router()
        .connected
        .lock()
        .iter()
        .filter(|n| **n == b.node_id())
        .count();
    let b_count = b
        .router()
        .connected
        .lock()
        .iter()
        .filter(|n| **n == a.node_id())
        .count();
    assert_eq!(a_count, 1, "A should record B connected exactly once");
    assert_eq!(b_count, 1, "B should record A connected exactly once");

    a.network().start_close();
    b.network().start_close();
    let _ = tokio::time::timeout(Duration::from_secs(10), a_dispatch).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), b_dispatch).await;
}
