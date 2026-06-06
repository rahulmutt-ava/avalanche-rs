// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `conformance::vm_battery` — the generic VM-conformance battery (specs 07 §10)
//! run against the in-memory `TestVm`, plus the non-batched `get_ancestors`
//! fallback byte-accounting test (`get_ancestors_fallback_limits`).
//!
//! Gated on the `testutil` feature (which exposes `TestVm` + the macro).

#![cfg(feature = "testutil")]
#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use ava_vm::block::{self, ChainVm};
use ava_vm::error::Error;
use ava_vm::testutil::{init_test_vm, TestVm};
use ava_vm::vm_conformance;

// The generic conformance battery, specialized to `TestVm`. `make_vm` returns an
// already-`initialize`d VM whose genesis is the last accepted block. The closure
// uses a fully-qualified path because the macro expands inside its own module.
vm_conformance!(|token: ::tokio_util::sync::CancellationToken| async move {
    ::ava_vm::testutil::init_test_vm(&token)
        .await
        .expect("init TestVm")
});

/// `get_ancestors` (non-batched fallback) respects `max_blocks_num`,
/// `max_blocks_size` (each element costs `len + INT_LEN`), the retrieval-time
/// bound, and treats `Err(NotFound)` on the head as an empty response.
#[tokio::test]
async fn get_ancestors_fallback_limits() {
    let token = CancellationToken::new();
    let mut vm = init_test_vm(&token).await.expect("init");

    // Build + accept a chain: genesis(0) <- b1 <- b2 <- b3 <- b4.
    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    let mut tip = vm.last_accepted(&token).await.expect("genesis");
    for _ in 0..4 {
        let blk = vm.build_block(&token).await.expect("build");
        blk.verify(&token).await.expect("verify");
        blk.accept(&token).await.expect("accept");
        vm.set_preference(&token, blk.id()).await.expect("pref");
        ids.push(blk.id());
        sizes.push(blk.bytes().len());
        tip = blk.id();
    }
    let head = tip; // b4

    // No bound hit: returns head + all 4 ancestors back to genesis = 5 blocks.
    let all = block::get_ancestors(
        &vm,
        &token,
        head,
        usize::MAX,
        usize::MAX,
        Duration::from_secs(60),
    )
    .await
    .expect("get_ancestors");
    assert_eq!(all.len(), 5, "head + 4 ancestors (down to genesis)");
    // First element is the head's own bytes.
    let head_blk = vm.get_block(&token, head).await.expect("head");
    assert_eq!(all[0], head_blk.bytes(), "first element is the head block");

    // max_blocks_num caps the count (the head is always element 0).
    let capped = block::get_ancestors(
        &vm,
        &token,
        head,
        2,
        usize::MAX,
        Duration::from_secs(60),
    )
    .await
    .expect("get_ancestors num cap");
    assert_eq!(capped.len(), 2, "max_blocks_num caps the result");

    // max_blocks_size: budget for exactly the head element only. The head is
    // added unconditionally; the first parent would push the total over the
    // budget, so it is excluded. Each element costs len + INT_LEN.
    let head_len = head_blk.bytes().len();
    let budget = head_len + block::INT_LEN; // exactly the head
    let size_capped = block::get_ancestors(
        &vm,
        &token,
        head,
        usize::MAX,
        budget,
        Duration::from_secs(60),
    )
    .await
    .expect("get_ancestors size cap");
    assert_eq!(
        size_capped.len(),
        1,
        "size budget admits only the head element"
    );

    // Budget for the head + exactly one parent.
    let parent_len = sizes[sizes.len() - 2]; // b3
    let budget2 = head_len + block::INT_LEN + parent_len + block::INT_LEN;
    let size_capped2 = block::get_ancestors(
        &vm,
        &token,
        head,
        usize::MAX,
        budget2,
        Duration::from_secs(60),
    )
    .await
    .expect("get_ancestors size cap 2");
    assert_eq!(size_capped2.len(), 2, "size budget admits head + one parent");

    // Unknown head id ⇒ empty response (signals the peer to stop asking).
    let unknown = ava_vm::Id::from([0x11u8; 32]);
    let empty = block::get_ancestors(
        &vm,
        &token,
        unknown,
        usize::MAX,
        usize::MAX,
        Duration::from_secs(60),
    )
    .await
    .expect("get_ancestors unknown head");
    assert!(empty.is_empty(), "unknown head ⇒ empty response");

    // Zero retrieval-time budget ⇒ only the head (the loop guard trips
    // immediately after the head is added).
    let timed = block::get_ancestors(
        &vm,
        &token,
        head,
        usize::MAX,
        usize::MAX,
        Duration::ZERO,
    )
    .await
    .expect("get_ancestors zero time");
    assert_eq!(timed.len(), 1, "zero retrieval time ⇒ head only");
}

/// `batched_parse_block` fallback parses one block at a time when the VM is not
/// batched, and surfaces a parse error.
#[tokio::test]
async fn batched_parse_block_fallback() {
    let token = CancellationToken::new();
    let mut vm = init_test_vm(&token).await.expect("init");
    let b1 = vm.build_block(&token).await.expect("b1");
    let b2 = vm.build_block(&token).await.expect("b2");
    let blobs = vec![b1.bytes().to_vec(), b2.bytes().to_vec()];

    let parsed = block::batched_parse_block(&vm, &token, &blobs)
        .await
        .expect("batched_parse_block fallback");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].id(), b1.id());
    assert_eq!(parsed[1].id(), b2.id());

    // A malformed blob propagates the parse error.
    let bad = vec![vec![0u8; 4]];
    assert!(matches!(
        block::batched_parse_block(&vm, &token, &bad).await,
        Err(Error::NotFound)
    ));
}

// Object-safety smoke: the engine holds `Arc<dyn ChainVm>`-shaped handles.
#[allow(dead_code)]
fn _object_safe(_: Arc<TestVm>, _: &dyn ChainVm) {}
