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

use ava_network::network::testutil::TestNetwork;
use ava_network::network::Network;

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
