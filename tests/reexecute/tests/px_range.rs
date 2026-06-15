// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `reexecute_px_range` — the P/X-Chain reexecute oracle (specs/02 §10.5).
//!
//! DEFERRED (M9.19 follow-up): no recorded P/X `blockexport`-style fixtures
//! exist in the repo yet. The P/X reexecute leg replays a recorded range of
//! mainnet P-Chain / X-Chain blocks from a fixed starting state and asserts the
//! resulting merkle roots match the Go-recorded expected roots. We will NOT
//! fabricate roots — this test is `#[ignore]`d until a Go-recorded P/X
//! `blockexport` fixture lands (mirrors how `ava-differential` defers its
//! absent-oracle live `interop` arm). See `tests/PORTING.md` for the follow-up.

// This deferred target names none of the crate's deps directly; per the
// established `unused_crate_dependencies` idiom it opts out of the per-binary
// false positive (same as the `ava-differential` integration targets).
#![allow(unused_crate_dependencies)]

#[test]
#[ignore = "pending P/X blockexport fixtures — M9.19 follow-up (no Go-recorded P/X roots in repo yet)"]
fn reexecute_px_range() {
    // Intentionally empty: when a P/X `blockexport` fixture is recorded, this
    // becomes a parity assertion mirroring `reexecute_cchain_range` (load the
    // recorded range, replay through the P/X VMs from the fixed start state,
    // assert each block's merkle root == the Go-recorded root).
    panic!("P/X reexecute fixtures not yet recorded — this test must be wired up with real Go-recorded roots before un-ignoring");
}
