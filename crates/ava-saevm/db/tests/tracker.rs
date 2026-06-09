// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Tracker ref-count / revision-window tests (specs/11 §7.1): `track`/`untrack`
//! bound retained revisions to the consensus-critical window, `state_db` opens
//! any retained revision, `last_height_with_execution_root_committed` rounds
//! down to the commit interval (or returns head if archival), and `close`
//! flattens to the last root (no reorgs).

mod common;

use ava_evm_reth::B256;
use ava_saevm_db::{Config, Tracker};

use crate::common::{open_provider, propose_root};

#[test]
fn track_untrack_refcount_bounds_revisions() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider.clone(), Config::archival());

    // Commit and track three execution roots (the consensus-critical window
    // LastExecuted..LastSettled). The retained count equals the live refs.
    let mut roots = Vec::new();
    for height in 1u64..=3 {
        let root = propose_root(&provider, height);
        tracker.maybe_commit(root, root, height).expect("commit");
        tracker.track(root);
        roots.push(root);
    }
    assert_eq!(tracker.retained_count(), 3, "three live references");

    // As consensus settles older blocks, the window slides forward: untrack the
    // oldest. The retained set shrinks — the window is bounded, not unbounded.
    tracker.untrack(roots[0]);
    assert_eq!(tracker.retained_count(), 2, "untrack drops one ref");

    // Untracking an untracked root is a no-op (saturates at zero), not a panic.
    tracker.untrack(roots[0]);
    assert_eq!(tracker.retained_count(), 2, "double-untrack is a no-op");

    // Double-track then double-untrack returns to a single retained ref.
    tracker.track(roots[1]);
    assert_eq!(tracker.retained_count(), 2, "refcount, not membership");
    tracker.untrack(roots[1]);
    assert_eq!(tracker.retained_count(), 2, "still referenced once");
    tracker.untrack(roots[1]);
    tracker.untrack(roots[2]);
    assert_eq!(tracker.retained_count(), 0, "window fully drained");
}

#[test]
fn state_db_opens_any_retained_revision() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider.clone(), Config::archival());

    // Commit a chain of roots; each committed (retained) revision is openable.
    let mut roots = Vec::new();
    for height in 1u64..=3 {
        let root = propose_root(&provider, height);
        tracker.maybe_commit(root, root, height).expect("commit");
        roots.push(root);
    }
    // Open the latest retained revision.
    tracker.state_db(roots[2]).expect("open tip revision");
    // Open an earlier retained revision (within Firewood's revision window).
    tracker
        .state_db(roots[1])
        .expect("open earlier retained revision");

    // A never-committed root is not retained ⇒ opening it errors.
    let bogus = B256::repeat_byte(0xab);
    assert!(
        tracker.state_db(bogus).is_err(),
        "unretained root is not openable"
    );
}

#[test]
fn last_height_with_execution_root_committed_rounds_down_to_interval() {
    let (_dir, provider) = open_provider();

    // Interval mode: the recovery start point rounds the head height DOWN to the
    // last commit-interval boundary.
    let tracker = Tracker::new(provider.clone(), Config::interval(4096));
    assert_eq!(
        tracker.last_height_with_execution_root_committed(0),
        0,
        "genesis"
    );
    assert_eq!(
        tracker.last_height_with_execution_root_committed(4095),
        0,
        "below first boundary"
    );
    assert_eq!(
        tracker.last_height_with_execution_root_committed(4096),
        4096,
        "exact boundary"
    );
    assert_eq!(
        tracker.last_height_with_execution_root_committed(10_000),
        8192,
        "rounds down to 2*interval"
    );

    // Archival mode: every block is committed ⇒ the head itself is the last
    // committed height.
    let archival = Tracker::new(provider, Config::archival());
    assert_eq!(
        archival.last_height_with_execution_root_committed(10_000),
        10_000,
        "archival returns head"
    );
}

#[test]
fn close_flattens_to_last_root() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider.clone(), Config::interval(4096));

    // Commit a couple of roots, then close to the last one. SAE has no reorgs,
    // so close flattens the snapshot to the last root unconditionally.
    let r1 = propose_root(&provider, 1);
    tracker.maybe_commit(r1, r1, 4096).expect("commit r1");

    let r2 = propose_root(&provider, 2);
    // r2 is proposed (stashed) but not yet durably committed (non-boundary).
    tracker.close(r2).expect("close flattens to last root");

    // After close, the durable tip is the flattened last root.
    assert_eq!(provider.root(), r2, "close commits the last root");
}
