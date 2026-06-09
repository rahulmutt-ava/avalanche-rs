// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Commit-policy tests for [`ava_saevm_db::Tracker::maybe_commit`] (specs/11
//! §7.1): archival commits every block, interval commits the settled root on a
//! boundary, otherwise the root stays in-memory and is still readable.

mod common;

use ava_saevm_db::{Config, Tracker};

use crate::common::{open_provider, propose_root};

#[test]
fn maybe_commit_archival_commits_every_block() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider.clone(), Config::archival());

    // Each block: propose a fresh execution root, then maybe_commit. Archival ⇒
    // every block's execution root is durably committed (tip advances to it).
    for height in 1u64..=3 {
        let exec_root = propose_root(&provider, height);
        tracker
            .maybe_commit(exec_root, exec_root, height)
            .expect("commit");
        assert_eq!(provider.root(), exec_root, "archival commits every block");
    }
}

#[test]
fn maybe_commit_interval_commits_settled_root_on_boundary() {
    let (_dir, provider) = open_provider();
    // Small interval so the test boundary is cheap.
    let tracker = Tracker::new(provider.clone(), Config::interval(4));

    let before = provider.root();

    // Non-boundary heights: nothing is committed (tip stays put).
    for height in 1u64..=3 {
        let exec_root = propose_root(&provider, height);
        tracker
            .maybe_commit(
                /* settled */ exec_root, /* execution */ exec_root, height,
            )
            .expect("maybe_commit");
        // We did not commit, so the stash for the unrelated root is dropped and
        // the tip is unchanged.
        assert_eq!(provider.root(), before, "non-boundary keeps tip");
    }

    // Boundary height (4 % 4 == 0): the SETTLED root is committed.
    let settled = propose_root(&provider, 4);
    tracker
        .maybe_commit(settled, propose_root(&provider, 99), 4)
        .expect("boundary commit");
    assert_eq!(provider.root(), settled, "boundary commits settled root");
}

#[test]
fn maybe_commit_else_keeps_root_in_memory_readable() {
    let (_dir, provider) = open_provider();
    let tracker = Tracker::new(provider.clone(), Config::interval(4096));

    // Commit a base revision so there is a retained tip to read from.
    let base = propose_root(&provider, 1);
    tracker.maybe_commit(base, base, 4096).expect("base commit");
    assert_eq!(provider.root(), base);

    // A non-boundary block: maybe_commit does NOT advance the tip. The committed
    // base root is still readable via state_db (the in-memory/retained window).
    let exec_root = propose_root(&provider, 7);
    tracker
        .maybe_commit(exec_root, exec_root, 7)
        .expect("non-boundary");
    assert_eq!(provider.root(), base, "non-boundary did not commit");

    // The retained committed revision is still openable.
    tracker.state_db(base).expect("retained revision readable");
}
