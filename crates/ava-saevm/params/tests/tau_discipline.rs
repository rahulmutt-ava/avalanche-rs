// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M7.2: the Tau discipline. A `BlockInstant` can only be moved by a
//! `Duration` (via `minus`/`plus`), never by raw-integer math — the structural
//! `tausecondslint` analog. Mirrors `vms/saevm/params/params.go` and
//! `vms/saevm/sae/block_builder.go::lastToSettle` (`bTime.Add(-saeparams.Tau)`).

use std::time::Duration;

use ava_saevm_params::{
    BlockInstant, LAMBDA, MAX_FULL_BLOCKS_IN_CLOSED_QUEUE, MAX_FULL_BLOCKS_IN_OPEN_QUEUE,
    MAX_FUTURE_BLOCK, MAX_QUEUE_WALL_TIME, TAU, TAU_SECONDS,
};

#[test]
fn tau_seconds_is_five() {
    assert_eq!(TAU_SECONDS, 5);
    assert_eq!(TAU, Duration::from_secs(5));
}

#[test]
fn block_instant_minus_tau() {
    // lastToSettle subtracts a `Duration` (Tau), never a raw integer.
    let settle = BlockInstant::from_unix(100).minus(TAU);
    assert_eq!(settle, BlockInstant::from_unix(95));

    // Saturates at the UNIX epoch instead of underflowing.
    let floored = BlockInstant::from_unix(2).minus(TAU);
    assert_eq!(floored, BlockInstant::from_unix(0));
}

#[test]
fn block_instant_plus_saturates_and_orders() {
    assert_eq!(
        BlockInstant::from_unix(10).plus(TAU),
        BlockInstant::from_unix(15)
    );
    assert!(BlockInstant::from_unix(10) < BlockInstant::from_unix(11));
    assert_eq!(BlockInstant::from_unix(7), BlockInstant::from_unix(7));
}

#[test]
fn max_queue_wall_time_is_duration_mul() {
    // params.go: MaxQueueWallTime = MaxFullBlocksInClosedQueue * Tau * Lambda
    //          = 3 * 5s * 2 = 30s.
    let multiplier =
        u32::try_from(MAX_FULL_BLOCKS_IN_CLOSED_QUEUE * LAMBDA).expect("multiplier fits in u32");
    assert_eq!(multiplier, 6);
    assert_eq!(MAX_QUEUE_WALL_TIME, TAU * multiplier);
    assert_eq!(MAX_QUEUE_WALL_TIME, Duration::from_secs(30));
}

#[test]
fn constant_values() {
    assert_eq!(LAMBDA, 2);
    assert_eq!(MAX_FULL_BLOCKS_IN_OPEN_QUEUE, 2);
    assert_eq!(MAX_FULL_BLOCKS_IN_CLOSED_QUEUE, 3);
    assert_eq!(MAX_FUTURE_BLOCK, Duration::from_secs(10));
}
