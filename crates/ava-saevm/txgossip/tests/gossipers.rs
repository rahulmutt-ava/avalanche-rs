// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Push/pull gossiper tokio-task kernels run against a fake [`GossipTransport`]
//! (specs/11 §9.2; `txgossip/gossip.go`). The live `Network::gossip` wiring is
//! deferred to M7.23 — these tests exercise the deterministic loop kernel.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ava_saevm_txgossip::{
    GossipTransport, PULL_GOSSIP_PERIOD, PUSH_GOSSIP_PERIOD, PullGossiper, PushGossiper,
};

/// Counts broadcast calls and the bytes seen; reports a fixed peer fan-out.
#[derive(Clone, Default)]
struct FakeTransport {
    calls: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

impl GossipTransport for FakeTransport {
    fn broadcast(&self, payload: Vec<u8>) -> usize {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bytes.fetch_add(payload.len(), Ordering::SeqCst);
        3 // pretend three peers received it
    }
}

#[test]
fn periods_match_go() {
    assert_eq!(PUSH_GOSSIP_PERIOD.as_millis(), 100);
    assert_eq!(PULL_GOSSIP_PERIOD.as_secs(), 1);
}

#[tokio::test(start_paused = true)]
async fn push_gossiper_broadcasts_each_nonempty_tick() {
    let t = FakeTransport::default();
    let g = PushGossiper::new(t.clone());
    // Tick 0 and 2 produce payloads; tick 1 produces nothing (no new txs).
    let mut tick = 0usize;
    let reached = g
        .run(3, move || {
            let p = if tick == 1 { None } else { Some(vec![0xab; 4]) };
            tick += 1;
            p
        })
        .await;
    assert_eq!(t.calls.load(Ordering::SeqCst), 2);
    assert_eq!(t.bytes.load(Ordering::SeqCst), 8);
    assert_eq!(reached, 6); // 2 broadcasts * 3 peers
}

#[tokio::test(start_paused = true)]
async fn pull_gossiper_issues_one_request_per_tick() {
    let t = FakeTransport::default();
    let g = PullGossiper::new(t.clone());
    let reached = g.run(4, vec![0x01, 0x02]).await;
    assert_eq!(t.calls.load(Ordering::SeqCst), 4);
    assert_eq!(reached, 12); // 4 ticks * 3 peers
}
