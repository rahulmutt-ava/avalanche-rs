// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `reexecute_px_range` — the P/X-Chain reexecute oracle (specs/02 §10.5/§11.1,
//! specs/16 §5(3), specs/00 §11.7).
//!
//! ## What this leg proves (X-Chain — DONE)
//!
//! No Go-recorded mainnet P/X `blockexport`-style fixture exists in the repo, so
//! — exactly as the C-Chain leg's `genesis_to_1` is a synthetic fixture run
//! through the REAL EVM pipeline — this leg builds a synthetic-but-real-pipeline
//! case: a seed-derived chain of X-Chain `BaseTx` issuances driven through the
//! REAL `ava-avm` VM/block pipeline (seed genesis state → admit txs → build →
//! verify → accept) via [`ava_reexecute::replay_xchain`]. The X-Chain keys UTXOs
//! by id (no merkle state — its block `MerkleRoot()` is the zero id), so the
//! reexecute "root" is the deterministic post-state digest: `sha256` over the
//! sorted final UTXO set, plus the chain-tip block id + height.
//!
//! The property asserted is the recorded-oracle property available WITHOUT a Go
//! oracle: **determinism / reproducibility**. The same case replayed on two
//! INDEPENDENT VM instances must produce byte-identical roots (specs/00 §6.1,
//! specs/02 §11). This is genuine VM execution — NOT a fabricated/hardcoded root
//! — mirroring how the `ava-differential` `xchain` collector proves determinism
//! when no live oracle exists. The Go recorded-oracle parity arm (compare against
//! a Go-EXECUTED P/X `blockexport` root) is the follow-up; see `tests/PORTING.md`.
//!
//! ## Deferred (P-Chain sub-leg + Go-oracle parity)
//!
//! The P-Chain sub-leg and the live Go recorded-oracle comparison are deferred —
//! see `tests/PORTING.md` for the precise as-built / deferred boundary.

// This integration target consumes only the `ava_reexecute` lib surface; the
// crate's VM/codec deps are linked by the lib, not named here. Per the
// established `unused_crate_dependencies` idiom each integration test that does
// not name them directly opts out of the per-binary false positive (see
// `cchain_range.rs`).
#![allow(unused_crate_dependencies)]

use ava_reexecute::replay_xchain;

/// A fixed seed selecting the synthetic X-Chain reexecute case. Any seed works;
/// pinning one keeps the case (chain length + per-tx amounts) reproducible across
/// runs and reviewers.
const CASE_SEED: u64 = 0x9019_5EED;

#[test]
fn reexecute_px_range() {
    // Replay the SAME synthetic case on two independent VM instances and assert
    // the resulting roots are byte-identical — the determinism / reproducibility
    // property the recorded-oracle path proves without a live Go oracle.
    let first = replay_xchain(CASE_SEED).expect("replay xchain reexecute case (1st)");
    let second = replay_xchain(CASE_SEED).expect("replay xchain reexecute case (2nd)");

    assert_eq!(
        first, second,
        "X-Chain reexecute roots must be deterministic across two independent replays of the same case"
    );

    // Non-trivial: the real pipeline accepted at least one block (height >= 1) and
    // the final post-state digest is a real sha256 (32 bytes), not a placeholder.
    assert!(
        first.last_accepted_height >= 1,
        "expected >=1 accepted block, got height {}",
        first.last_accepted_height
    );
    assert_eq!(
        first.state_digest.len(),
        32,
        "post-state digest must be a 32-byte sha256"
    );
    assert_ne!(
        first.state_digest, [0u8; 32],
        "post-state digest must be a real (non-zero) sha256 over the final UTXO set"
    );

    // A different case (different seed) must (with overwhelming probability)
    // produce a different root — proves the assertion genuinely catches divergence
    // rather than passing on a constant.
    let other = replay_xchain(CASE_SEED ^ 0xFFFF_FFFF).expect("replay xchain reexecute case (alt)");
    assert_ne!(
        first, other,
        "a different synthetic case must produce a different reexecute root"
    );
}
