// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The SAE §10 invariant harness (specs/11 §10, invariants 1–11).
//!
//! One named test per invariant, all under a `mod invariant { … }` so the
//! M7.32 exit gate can select the whole set with
//! `cargo nextest run -p ava-saevm-core -E 'test(invariant)'`. Each test drives
//! a scenario through the reusable harness in
//! [`ava_saevm_testutil::invariants`] (build → verify → accept → execute →
//! settle → restart) and asserts one of the eleven properties from specs/11
//! §10:
//!
//! 1. **Frontier ordering** — `height(S) <= height(E) <= height(A)`.
//! 2. **Stage causality** — `b∈S ⇒ b∈E ⇒ b∈A`.
//! 3. **Persistence ordering on execute** — `D → M → I → X`; a reader woken by
//!    `X` always observes `D`.
//! 4. **Persistence ordering on accept** — settled hash persisted before the
//!    canonical/accepted hash.
//! 5. **Settle-in-order** — ancestors settled in increasing height.
//! 6. **Atomics-before-broadcast** — the internal pointer is advanced before
//!    the `WaitUntil{Executed,Settled}` notify fires.
//! 7. **Recovery equivalence** — restart reconstructs identical A/E/S +
//!    post-state roots (delegates to M7.24 `recover()`).
//! 8. **GC of settled ancestry** — after settle, `parent()`/`last_settled()` →
//!    `None` and `InMemoryBlockCount` returns to baseline.
//! 9. **No reorg** — acceptance is final.
//! 10. **Receipt-root match** — stored `receipt_root == derive_sha(receipts)`.
//! 11. **Determinism** — execution output is independent of wall-clock + map
//!     order (delegates to M7.16 `prop::sae_execution_determinism`; re-asserted
//!     here at the chain level).

mod invariant {
    use ava_saevm_testutil::invariants as harness;

    /// (1) `height(S) <= height(E) <= height(A)` holds at every step of a live
    /// chain.
    #[tokio::test]
    async fn frontier_ordering() {
        harness::assert_frontier_ordering(8).await;
    }

    /// (2) Settled ⇒ Executed ⇒ Accepted for every block on the chain.
    #[tokio::test]
    async fn stage_causality() {
        harness::assert_stage_causality(8).await;
    }

    /// (3) `mark_executed` runs `D → M → I → X`; a reader woken by `X` can read
    /// the persisted artefacts (`D`).
    #[tokio::test]
    async fn persist_order_execute() {
        harness::assert_persist_order_execute().await;
    }

    /// (4) The settled hash is persisted before the canonical/accepted hash.
    #[tokio::test]
    async fn persist_order_accept() {
        harness::assert_persist_order_accept(6).await;
    }

    /// (5) Ancestors are settled in strictly increasing height.
    #[tokio::test]
    async fn settle_in_order() {
        harness::assert_settle_in_order(10).await;
    }

    /// (6) The internal frontier pointer is advanced before the broadcast notify
    /// fires (poll never sees a stale pointer).
    #[tokio::test]
    async fn atomics_before_broadcast() {
        harness::assert_atomics_before_broadcast().await;
    }

    /// (7) Restart reconstructs identical A/E/S + post-state roots.
    #[tokio::test]
    async fn recovery_equivalence() {
        harness::assert_recovery_equivalence(8).await;
    }

    /// (8) After settle, `parent()`/`last_settled()` → `None`; the in-memory
    /// block count returns to its baseline (no ancestry leak).
    #[tokio::test]
    async fn gc_settled_ancestry() {
        harness::assert_gc_settled_ancestry().await;
    }

    /// (9) Acceptance is final — no reorg; the canonical id at each height is
    /// stable across snapshot flattening.
    #[tokio::test]
    async fn no_reorg() {
        harness::assert_no_reorg(6).await;
    }

    /// (10) The stored `receipt_root` equals `derive_sha(receipts)`.
    #[test]
    fn receipt_root_match() {
        harness::assert_receipt_root_match();
    }

    /// (11) Execution output is independent of wall-clock + map order.
    #[tokio::test]
    async fn determinism() {
        harness::assert_determinism(8).await;
    }
}
