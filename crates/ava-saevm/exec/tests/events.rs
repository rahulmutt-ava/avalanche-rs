// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Async-reactor (M7.15) tests: the `Eventual<T>` receipt buffer, chain-head
//! broadcast, and the `WaitUntil{Executed,Settled}` ordering contract
//! (specs/11 §6, §10 invariant 6), plus graceful shutdown drain.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ava_saevm_exec::{ChainHeadEvent, Eventual, ExecutionWaiters, HeadEvents, TxReceipt};
use ava_saevm_types::B256;
use ava_vm::components::gas::Price;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

fn receipt(tag: u8) -> TxReceipt {
    TxReceipt {
        tx_hash: B256::repeat_byte(tag),
        gas_used: 21_000,
        effective_gas_price: Price(7),
        reverted: false,
    }
}

/// Invariant 6 (atomics-before-broadcast): a waiter woken by the `executed`
/// notify always reads the advanced internal pointer — never a stale one.
#[tokio::test]
async fn wait_until_executed_observes_pointer_first() {
    let waiters = Arc::new(ExecutionWaiters::new());
    // Shared "internal pointer" the waiter reads after being woken.
    let pointer = Arc::new(AtomicU64::new(0));

    let target_height = 10u64;
    let w = Arc::clone(&waiters);
    let p = Arc::clone(&pointer);
    let waiter = tokio::spawn(async move {
        w.wait_until_executed(target_height).await;
        // Once woken, the pointer MUST be >= the height the broadcast announced.
        p.load(Ordering::SeqCst)
    });

    // Advance the pointer BEFORE firing the notify (invariant 6 ordering).
    tokio::task::yield_now().await;
    pointer.store(target_height, Ordering::SeqCst);
    waiters.set_executed(target_height);

    let observed = waiter.await.expect("waiter task panicked");
    assert!(
        observed >= target_height,
        "woken waiter read a stale pointer ({observed} < {target_height})"
    );
}

/// A waiter on a tx hash resolves with the published receipt; set-once
/// semantics hold (resolve twice / publish-before-wait both work).
#[tokio::test]
async fn receipt_eventual_resolves_after_publish() {
    let ev: Eventual<TxReceipt> = Eventual::new();

    // Spawn a waiter before the value is published.
    let ev2 = ev.clone();
    let waiter = tokio::spawn(async move { ev2.wait().await });

    tokio::task::yield_now().await;
    assert!(ev.set(receipt(1)));

    let got = waiter.await.expect("waiter task panicked");
    assert_eq!(got, receipt(1));

    // Set-once: a second set is rejected and the value is unchanged.
    assert!(!ev.set(receipt(2)));
    assert_eq!(ev.get(), Some(receipt(1)));

    // Publish-before-wait resolves immediately with the first value.
    assert_eq!(ev.wait().await, receipt(1));
}

/// One `ChainHeadEvent` is received per block, in height order.
#[tokio::test]
async fn subscribe_chain_head_receives_event_per_block() {
    let head = HeadEvents::new();
    let mut rx = head.subscribe_chain_head();

    let n = 5u64;
    for height in 1..=n {
        head.emit(ChainHeadEvent {
            height,
            hash: B256::repeat_byte(u8::try_from(height).expect("height fits u8")),
        });
    }

    for height in 1..=n {
        let ev = rx.recv().await.expect("chain-head event missing");
        assert_eq!(ev.height, height);
        assert_eq!(
            ev.hash,
            B256::repeat_byte(u8::try_from(height).expect("height fits u8"))
        );
    }
}

/// Spawned tasks under a `TaskTracker` + `CancellationToken` drain on shutdown:
/// in-flight work finishes before `tracker.wait()` completes.
#[tokio::test]
async fn task_tracker_drains_on_shutdown() {
    let tracker = TaskTracker::new();
    let token = CancellationToken::new();
    let finished = Arc::new(AtomicU64::new(0));

    for _ in 0..4u8 {
        let t = token.clone();
        let f = Arc::clone(&finished);
        tracker.spawn(async move {
            // Real spawned task: wait for cancellation, then finish in-flight work.
            t.cancelled().await;
            f.fetch_add(1, Ordering::SeqCst);
        });
    }

    tracker.close();
    token.cancel();
    tracker.wait().await;

    assert_eq!(
        finished.load(Ordering::SeqCst),
        4,
        "not all in-flight tasks finished before drain completed"
    );
    assert!(tracker.is_empty(), "tracker leaked tasks after wait()");
}
