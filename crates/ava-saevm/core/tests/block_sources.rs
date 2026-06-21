// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! SAE VM `GetBlock` source-resolution tests (specs/11 §4 upstream-delta;
//! Go `vms/saevm/sae/vm_test.go::TestBlockSources`, PR #5547 / `84533ec5b1`).
//!
//! Mirrors the Go fix where `VM.GetBlock` switched from `return b, nil` (which
//! silently swallowed any non-`ErrNotFound` read error) to `return b, err` so
//! that a corrupt / failed settled-block DB read **surfaces the underlying read
//! error** rather than masquerading as a not-found miss.
//!
//! The Rust analogue: [`ava_saevm_core::Vm::block_by_id`] consults the in-memory
//! consensus-critical store first, then falls through to the optional
//! [`SettledBlockSource`] seam (the Go `settledBlockFromDB` `fromDB` reader).
//! A genuine absence (`Ok(None)`) maps to [`Error::NotFound`]; any other read
//! failure (`Err(..)`) propagates as the real underlying error.

#![allow(clippy::arithmetic_side_effects)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_adaptor::ChainVm;
use ava_saevm_blocks::Block;
use ava_saevm_core::{
    BlockBuilderSeam, BuildError, Error, ExecutorSeam, SaeBlock, SettledBlockSource,
    SettledReadError, Vm,
};
use ava_types::id::Id;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn eth_block(number: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

/// A genesis (synchronous, self-settling) SAE block at height 0.
fn genesis() -> Arc<Block> {
    let g = Arc::new(Block::new(eth_block(0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

/// A bare block at `number` on top of `parent` — stands in for a settled
/// ancestor that lives only in the DB (the Go below-`S` case).
fn settled_descendant(number: u64, parent: &Arc<Block>) -> Arc<Block> {
    Arc::new(
        Block::new(
            eth_block(number, parent.hash()),
            Some(Arc::clone(parent)),
            None,
        )
        .expect("descendant"),
    )
}

// ---------------------------------------------------------------------------
// Inert seams (this test exercises only the read path)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct InertBuilder;

impl BlockBuilderSeam for InertBuilder {
    fn build_on(&self, _parent: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Err(BuildError::Builder("unused".into()))
    }

    fn rebuild(&self, _parent: &Arc<Block>, _b: &Arc<Block>) -> Result<Arc<Block>, BuildError> {
        Err(BuildError::Builder("unused".into()))
    }
}

#[derive(Default)]
struct InertExecutor;

impl ExecutorSeam for InertExecutor {
    fn enqueue(&self, _block: &Arc<Block>) -> Result<(), BuildError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Corruptable settled-block source (the Go `corruptableHeightIndex` analogue)
// ---------------------------------------------------------------------------

/// The injected read error returned for a corrupted hash (Go `errInjectedRead`).
#[derive(Debug, thiserror::Error)]
#[error("injected read error")]
struct InjectedRead;

/// A DB-backed settled-block source that can be told to fail the read for a
/// specific hash (mirrors `corruptableHeightIndex.corrupt`). Found blocks return
/// `Ok(Some(_))`, genuine misses `Ok(None)`, a corrupted hash `Err(InjectedRead)`.
#[derive(Default)]
struct CorruptableSource {
    blocks: Mutex<Vec<SaeBlock>>,
    corrupted: Mutex<Option<B256>>,
}

impl CorruptableSource {
    fn insert(&self, handle: SaeBlock) {
        self.blocks.lock().push(handle);
    }

    fn corrupt(&self, hash: B256) {
        *self.corrupted.lock() = Some(hash);
    }
}

impl SettledBlockSource for CorruptableSource {
    fn settled_block(&self, hash: B256) -> Result<Option<SaeBlock>, SettledReadError> {
        if *self.corrupted.lock() == Some(hash) {
            return Err(Box::new(InjectedRead));
        }
        let found = self
            .blocks
            .lock()
            .iter()
            .find(|b| b.block().hash() == hash)
            .cloned();
        Ok(found)
    }
}

// ---------------------------------------------------------------------------
// VM construction
// ---------------------------------------------------------------------------

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000)
}

fn id_of(b: &Arc<Block>) -> Id {
    Id::from(b.hash().0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A genuinely absent block still yields [`Error::NotFound`] (the baseline that
/// the swallow-fix must not regress) — both via the in-memory-only VM and via a
/// VM whose settled source reports `Ok(None)`.
#[tokio::test]
async fn get_block_absent_is_not_found() {
    let g = genesis();
    let source = Arc::new(CorruptableSource::default());
    let vm = Arc::new(Vm::with_settled_source(
        &g,
        InertBuilder,
        Arc::new(InertExecutor),
        now,
        Arc::clone(&source) as Arc<dyn SettledBlockSource>,
    ));

    // A hash neither in memory nor in the (empty) source.
    let phantom = Id::from([0xAB; 32]);
    // `SaeBlock` is not `Debug`, so unwrap the error via `let-else` rather than
    // `Result::expect_err` (which would require the `Ok` type to be `Debug`).
    let Err(err) = vm.block_by_id(phantom) else {
        panic!("absent block must be NotFound, got Ok");
    };
    assert!(
        matches!(err, Error::NotFound),
        "genuine absence maps to NotFound, got: {err}",
    );

    // The trait-level get_block flattens to the ava_vm sentinel but must not Ok.
    let Err(err) = vm.get_block(phantom).await else {
        panic!("absent block must error at the ChainVm boundary, got Ok");
    };
    assert!(
        matches!(err, ava_vm::Error::NotFound),
        "absent block at the boundary is NotFound, got: {err}",
    );
}

/// A genuine in-memory hit (genesis) resolves without ever touching the source.
#[tokio::test]
async fn get_block_in_memory_hit() {
    let g = genesis();
    let source = Arc::new(CorruptableSource::default());
    let vm = Arc::new(Vm::with_settled_source(
        &g,
        InertBuilder,
        Arc::new(InertExecutor),
        now,
        Arc::clone(&source) as Arc<dyn SettledBlockSource>,
    ));

    let got = vm.block_by_id(id_of(&g)).expect("genesis is in memory");
    assert_eq!(got.block().hash(), g.hash(), "genesis resolved from memory");
}

/// A settled ancestor evicted from memory but present in the DB source resolves
/// through the seam fallthrough (Go below-`S` / settled-in-DB case).
#[tokio::test]
async fn get_block_settled_in_db_hit() {
    let g = genesis();
    let on_disk = settled_descendant(1, &g);
    let source = Arc::new(CorruptableSource::default());
    source.insert(SaeBlock::new(Arc::clone(&on_disk)));

    let vm = Arc::new(Vm::with_settled_source(
        &g,
        InertBuilder,
        Arc::new(InertExecutor),
        now,
        Arc::clone(&source) as Arc<dyn SettledBlockSource>,
    ));

    let got = vm
        .block_by_id(id_of(&on_disk))
        .expect("settled block resolves from the DB source");
    assert_eq!(
        got.block().hash(),
        on_disk.hash(),
        "settled-in-DB block resolved through the seam",
    );
}

/// The regression: a corrupt / failed settled-block read must **surface the
/// injected read error**, NOT collapse to `NotFound` and NOT return `Ok`.
///
/// This is exactly Go `TestBlockSources`'s `corrupted` case
/// (`wantGetBlockErr: testerr.Is(errInjectedRead)`).
#[tokio::test]
async fn get_block_corrupt_read_surfaces_underlying_error() {
    let g = genesis();
    let corrupted = settled_descendant(1, &g);
    let source = Arc::new(CorruptableSource::default());
    // The block IS in the DB, but the read for its hash is poisoned.
    source.insert(SaeBlock::new(Arc::clone(&corrupted)));
    source.corrupt(corrupted.hash());

    let vm = Arc::new(Vm::with_settled_source(
        &g,
        InertBuilder,
        Arc::new(InertExecutor),
        now,
        Arc::clone(&source) as Arc<dyn SettledBlockSource>,
    ));

    let Err(err) = vm.block_by_id(id_of(&corrupted)) else {
        panic!("corrupt read must error, not NotFound, not Ok");
    };

    // It must NOT be the not-found sentinel (the bug being fixed).
    assert!(
        !matches!(err, Error::NotFound),
        "corrupt read must NOT be swallowed as NotFound, got: {err}",
    );

    // It must be the SettledRead variant, and the underlying source error must
    // survive the error chain (the Rust analogue of Go's `%v`->`%w`).
    match &err {
        Error::SettledRead(source) => {
            assert!(
                source.downcast_ref::<InjectedRead>().is_some(),
                "underlying InjectedRead must survive the error chain, got: {source}",
            );
        }
        other => panic!("corrupt read must surface SettledRead, got: {other}"),
    }
}
