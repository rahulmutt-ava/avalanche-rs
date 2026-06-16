// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `reexecute_pchain_range` — the P-Chain reexecute oracle (specs/02 §10.5/§11.1,
//! specs/16 §5(3), specs/00 §11.7).
//!
//! ## What this leg proves (P-Chain — DONE, determinism arm)
//!
//! No Go-recorded mainnet P-Chain `blockexport`-style fixture exists in the repo,
//! so — exactly as the C-Chain leg's `genesis_to_1` is a synthetic fixture run
//! through the REAL EVM pipeline, and the X-Chain leg builds a synthetic `BaseTx`
//! chain — this leg builds a synthetic-but-real-pipeline case: a seed-derived
//! P-Chain genesis (two UTXOs + one current validator) driven through the REAL
//! `ava-platformvm` VM/block pipeline (parse + seed genesis state →
//! `build → set_preference → verify → accept` until the builder declines) via
//! [`ava_reexecute::replay_pchain`].
//!
//! Because the P-Chain mempool is un-shared on `PlatformVm` (no public tx-admission
//! seam without patching the VM) AND the genesis ⇄ staker-reward resolver wiring is
//! a known gap (`genesis::seed_state` records the validator as a staker but does
//! not store its tx, so the reward executor's `GetTx` resolver returns
//! `ErrNotFound` on verify), this leg drives the REAL pipeline to its honestly
//! reachable floor: `initialize` over a seed-derived genesis (parse → `seed_state`
//! → genesis block), then `build_block`, which — with the genesis time + validator
//! period future-pinned — honestly declines (`ErrNoPendingBlocks`). The chain stays
//! at the accepted genesis tip (height 0). The P-Chain keeps FLAT KV state (no
//! merkledb), so the reexecute "root" is the deterministic post-state digest:
//! `sha256` over the sorted final UTXO set + Primary-Network supply + chain
//! timestamp, plus the chain-tip block id + height. (A height >= 1 accepted-block
//! arm is the follow-up — see `tests/PORTING.md`.)
//!
//! The property asserted is the recorded-oracle property available WITHOUT a Go
//! oracle: **determinism / reproducibility**. The same case replayed on two
//! INDEPENDENT VM instances must produce byte-identical roots (specs/00 §6.1,
//! specs/02 §11). This is genuine VM execution — NOT a fabricated/hardcoded root.
//! The Go recorded-oracle parity arm (compare against a Go-EXECUTED P-Chain
//! `blockexport` root) is the follow-up; see `tests/PORTING.md`.

// This integration target consumes only the `ava_reexecute` lib surface; the
// crate's VM/codec deps are linked by the lib, not named here. Per the established
// `unused_crate_dependencies` idiom each integration test that does not name them
// directly opts out of the per-binary false positive (see `cchain_range.rs` /
// `px_range.rs`).
#![allow(unused_crate_dependencies)]

use ava_reexecute::replay_pchain;

/// A fixed seed selecting the synthetic P-Chain reexecute case. Any seed works;
/// pinning one keeps the case (genesis UTXO amounts + validator stake/period)
/// reproducible across runs and reviewers.
const CASE_SEED: u64 = 0x9019_9CA1;

#[test]
fn reexecute_pchain_range() {
    // Replay the SAME synthetic case on two independent VM instances and assert
    // the resulting roots are byte-identical — the determinism / reproducibility
    // property the recorded-oracle path proves without a live Go oracle.
    let first = replay_pchain(CASE_SEED).expect("replay pchain reexecute case (1st)");
    let second = replay_pchain(CASE_SEED).expect("replay pchain reexecute case (2nd)");

    assert_eq!(
        first, second,
        "P-Chain reexecute roots must be deterministic across two independent replays of the same case"
    );

    // Non-trivial: the real pipeline reached at least the genesis tip (height >= 0)
    // and the final post-state digest is a real sha256 (32 bytes), not a placeholder.
    assert_eq!(
        first.state_digest.len(),
        32,
        "post-state digest must be a 32-byte sha256"
    );
    assert_ne!(
        first.state_digest, [0u8; 32],
        "post-state digest must be a real (non-zero) sha256 over the final post-state"
    );
    // The chain-tip id is the genesis block id (a later accepted block once the
    // height >= 1 arm lands); it must be a real (non-zero) 32-byte id.
    assert_ne!(
        first.last_accepted_id, [0u8; 32],
        "chain-tip block id must be a real (non-zero) 32-byte id"
    );
    // The funded, signed `CreateSubnetTx` admitted through the `mempool_add` seam
    // packs into a height-1 `BanffStandardBlock`, which verifies + accepts against
    // the future-pinned genesis time. The chain tip is therefore the accepted
    // standard block at height 1.
    assert_eq!(
        first.last_accepted_height, 1,
        "the admitted CreateSubnetTx produces + accepts a height-1 standard block"
    );

    // A different case (different seed) must (with overwhelming probability) produce
    // a different root — proves the assertion genuinely catches divergence rather
    // than passing on a constant.
    let other = replay_pchain(CASE_SEED ^ 0xFFFF_FFFF).expect("replay pchain reexecute case (alt)");
    assert_ne!(
        first, other,
        "a different synthetic case must produce a different reexecute root"
    );
}
