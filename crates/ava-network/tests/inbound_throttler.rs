// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the inbound message byte throttler and the inbound
//! connection-upgrade throttler. Ports `inbound_msg_byte_throttler_test.go`
//! and `inbound_conn_upgrade_throttler_test.go` (happy path + the fairness /
//! cooldown invariants exercised in the plan).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use ava_network::throttling::inbound_conn_upgrade::InboundConnUpgradeThrottler;
use ava_network::throttling::inbound_msg_byte::InboundMsgByteThrottler;
use ava_types::node_id::NodeId;
use tokio::time::{Duration as TokioDuration, timeout};
use tokio_util::sync::CancellationToken;

fn node(b: u8) -> NodeId {
    let mut raw = [0u8; 20];
    raw[0] = b;
    NodeId::from_slice(&raw).unwrap()
}

/// Fill the at-large pool with a first acquire; a second acquire for the same
/// budget must block until the first permit is dropped, then proceed.
#[tokio::test]
async fn acquire_blocks_then_releases() {
    // at-large = 1024, node-max = 1024, no validator allocation.
    let throttler = InboundMsgByteThrottler::new(0, 1024, 1024);
    let cancel = CancellationToken::new();

    let node_a = node(1);
    let node_b = node(2);

    // First acquire takes the whole at-large pool.
    let permit1 = throttler
        .acquire(1024, node_a, &cancel)
        .await
        .expect("first acquire should succeed");

    // Second acquire (different node) needs 512 bytes but the pool is empty.
    let throttler2 = throttler.clone();
    let cancel2 = cancel.clone();
    let handle = tokio::spawn(async move { throttler2.acquire(512, node_b, &cancel2).await });

    // The second acquire must not complete while permit1 is held.
    let blocked = timeout(TokioDuration::from_millis(100), async {
        // Peek without consuming the JoinHandle.
    })
    .await;
    assert!(blocked.is_ok());
    assert!(
        !handle.is_finished(),
        "second acquire must block while pool full"
    );

    // Dropping permit1 returns 1024 bytes; the waiter wakes and completes.
    drop(permit1);

    let permit2 = timeout(TokioDuration::from_secs(5), handle)
        .await
        .expect("second acquire should not time out")
        .expect("join should not panic")
        .expect("second acquire should succeed after release");
    drop(permit2);
}

/// A node with one outstanding (blocked) acquire cannot prevent another node
/// from acquiring from the pool.
#[tokio::test]
async fn per_node_single_outstanding_acquire() {
    // at-large = 1024, node-max = 1024.
    let throttler = InboundMsgByteThrottler::new(0, 1024, 1024);
    let cancel = CancellationToken::new();

    let node_a = node(1);
    let node_b = node(2);

    // node_a drains the whole pool.
    let permit_a = throttler
        .acquire(1024, node_a, &cancel)
        .await
        .expect("node_a acquire should succeed");

    // node_a issues a second acquire that blocks (one outstanding per node).
    let t2 = throttler.clone();
    let c2 = cancel.clone();
    let a2 = node_a;
    let blocked_a = tokio::spawn(async move { t2.acquire(256, a2, &c2).await });

    // Give the blocked acquire a moment to register as waiting.
    tokio::task::yield_now().await;
    assert!(!blocked_a.is_finished());

    // Releasing node_a's permit frees the pool. node_b can now acquire.
    drop(permit_a);

    // node_a's waiting acquire takes 256 of the freed 1024 first (fairness:
    // oldest waiter served), leaving 768 for node_b.
    let permit_a2 = timeout(TokioDuration::from_secs(5), blocked_a)
        .await
        .unwrap()
        .unwrap()
        .expect("node_a's queued acquire should succeed");

    let permit_b = timeout(
        TokioDuration::from_secs(5),
        throttler.acquire(512, node_b, &cancel),
    )
    .await
    .expect("node_b acquire should not time out")
    .expect("node_b should be able to acquire");

    drop(permit_a2);
    drop(permit_b);
}

/// Same IP within the cooldown window is rejected; after the cooldown elapses
/// it is allowed again. Uses the injectable clock (`should_upgrade_at`).
#[test]
fn conn_upgrade_cooldown_rejects() {
    let cooldown = Duration::from_secs(10);
    let throttler = InboundConnUpgradeThrottler::new(cooldown, 256);

    let ip: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4));
    let other: IpAddr = IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8));

    let t0 = Instant::now();

    // First attempt: allowed.
    assert!(throttler.should_upgrade_at(ip, t0));
    // Immediate retry from the same IP: rejected (within cooldown).
    assert!(!throttler.should_upgrade_at(ip, t0 + Duration::from_secs(1)));
    assert!(!throttler.should_upgrade_at(ip, t0 + Duration::from_secs(9)));
    // A different IP is unaffected.
    assert!(throttler.should_upgrade_at(other, t0 + Duration::from_secs(1)));

    // After the cooldown elapses, the original IP is allowed again.
    assert!(throttler.should_upgrade_at(ip, t0 + Duration::from_secs(11)));

    // Loopback is never rate-limited.
    let loop_ip: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(throttler.should_upgrade_at(loop_ip, t0));
    assert!(throttler.should_upgrade_at(loop_ip, t0));
}

/// The global rate limit caps the number of upgrades within one cooldown
/// window regardless of distinct IPs.
#[test]
fn conn_upgrade_global_rate_limit() {
    let cooldown = Duration::from_secs(10);
    let max = 2;
    let throttler = InboundConnUpgradeThrottler::new(cooldown, max);

    let t0 = Instant::now();
    assert!(throttler.should_upgrade_at(node_ip(1), t0));
    assert!(throttler.should_upgrade_at(node_ip(2), t0));
    // Third distinct IP within the window exceeds the global cap.
    assert!(!throttler.should_upgrade_at(node_ip(3), t0));

    // After the window, capacity is restored.
    assert!(throttler.should_upgrade_at(node_ip(3), t0 + Duration::from_secs(11)));
}

fn node_ip(b: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, 0, 0, b))
}
