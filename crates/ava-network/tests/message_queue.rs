// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

//! M2.12 — outbound `MessageQueue` (throttled + blocking) + outbound byte
//! throttler RAII permits (`specs/05` §3.3, §5).

use ava_message::codec::OutboundMessage;
use ava_message::ops::Op;
use ava_network::peer::message_queue::{MessageQueue, ThrottledMessageQueue};
use ava_network::throttling::outbound_msg::{OutboundMsgThrottler, OutboundMsgThrottlerConfig};
use ava_types::node_id::NodeId;
use bytes::Bytes;

fn node(byte: u8) -> NodeId {
    NodeId::from_slice(&[byte; 20]).unwrap()
}

/// Builds a minimal outbound message with a payload of `len` bytes.
fn msg(len: usize, bypass: bool) -> OutboundMessage {
    OutboundMessage {
        bypass_throttling: bypass,
        op: Op::Ping,
        bytes: Bytes::from(vec![0u8; len]),
        bytes_saved_compression: 0,
    }
}

#[tokio::test]
async fn throttled_queue_push_pop_fifo() {
    let throttler = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
    let queue = ThrottledMessageQueue::new(throttler, node(1));

    assert!(queue.push(msg(10, false)));
    assert!(queue.push(msg(20, false)));
    assert!(queue.push(msg(30, false)));

    assert_eq!(queue.pop().await.unwrap().bytes.len(), 10);
    assert_eq!(queue.pop().await.unwrap().bytes.len(), 20);
    assert_eq!(queue.pop().await.unwrap().bytes.len(), 30);
}

#[tokio::test]
async fn bypass_throttling_skips_acquire() {
    // A tiny at-large + node-max pool so a single normal message exhausts it.
    let cfg = OutboundMsgThrottlerConfig {
        vdr_alloc_size: 0,
        at_large_alloc_size: 16,
        node_max_at_large_bytes: 16,
    };
    let throttler = OutboundMsgThrottler::new(cfg);
    let queue = ThrottledMessageQueue::new(throttler, node(1));

    // Exhaust the throttler with a 16-byte normal message.
    assert!(queue.push(msg(16, false)));
    // A further normal message is refused (dropped).
    assert!(!queue.push(msg(16, false)));
    // A bypass message skips acquire and is still enqueued.
    assert!(queue.push(msg(16, true)));
}

#[tokio::test]
async fn drop_releases_throttler_bytes() {
    let cfg = OutboundMsgThrottlerConfig {
        vdr_alloc_size: 0,
        at_large_alloc_size: 16,
        node_max_at_large_bytes: 16,
    };
    let throttler = OutboundMsgThrottler::new(cfg);
    let queue = ThrottledMessageQueue::new(throttler, node(1));

    // Consume all 16 bytes.
    assert!(queue.push(msg(16, false)));
    // Pool exhausted: next normal push refused.
    assert!(!queue.push(msg(16, false)));

    // Popping releases the RAII permit, returning the bytes.
    let popped = queue.pop().await.unwrap();
    assert_eq!(popped.bytes.len(), 16);
    drop(popped);

    // The bytes are available again — a fresh 16-byte message is accepted.
    assert!(queue.push(msg(16, false)));
}

#[tokio::test]
async fn pop_now_non_blocking() {
    let throttler = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
    let queue = ThrottledMessageQueue::new(throttler, node(1));

    assert!(queue.pop_now().is_none());
    assert!(queue.push(msg(5, false)));
    assert_eq!(queue.pop_now().unwrap().bytes.len(), 5);
    assert!(queue.pop_now().is_none());
}

#[tokio::test]
async fn close_drains_then_returns_none() {
    let throttler = OutboundMsgThrottler::new(OutboundMsgThrottlerConfig::default());
    let queue = ThrottledMessageQueue::new(throttler, node(1));

    assert!(queue.push(msg(1, false)));
    queue.close();
    // Push after close is rejected.
    assert!(!queue.push(msg(1, false)));
    // Closed queue drains the remaining message, then returns None.
    assert_eq!(queue.pop().await.unwrap().bytes.len(), 1);
    assert!(queue.pop().await.is_none());
}
