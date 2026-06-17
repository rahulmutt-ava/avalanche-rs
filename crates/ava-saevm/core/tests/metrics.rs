// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `sae`-namespace prometheus metric registration + scrape tests (specs/18
//! §2.11; Go `sae/metrics.go`).
//!
//! Registers a [`SaeMetrics`] into a fresh [`prometheus::Registry`], drives the
//! [`Frontier`] / in-memory-block counter, and gathers — asserting the exposed
//! metric families carry the expected names and the values track the live
//! backing stores (the `GaugeFunc` scrape-time sampling property).

// Readable reference arithmetic + small-index casts in the chain builders + an
// f64→i64 gauge-value read; the values are tiny and exact, so no truncation.
#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_evm_reth::{B256, Header, RethBlock, SealedBlock};
use ava_saevm_blocks::{Block, ExecutionArtefacts, in_memory_block_count};
use ava_saevm_core::{Frontier, SaeMetrics, settle};
use ava_saevm_proxytime::Time;
use ava_saevm_types::ExecutionResults;
use ava_vm::components::gas::Price;
use prometheus::Registry;
use prometheus::proto::MetricFamily;

// ---------------------------------------------------------------------------
// Chain builders (mirror core/tests/frontier.rs).
// ---------------------------------------------------------------------------

fn eth_block(number: u64, timestamp: u64, parent_hash: B256) -> SealedBlock<RethBlock> {
    let header = Header {
        parent_hash,
        number,
        timestamp,
        ..Header::default()
    };
    SealedBlock::seal_slow(RethBlock::uncle(header))
}

fn genesis() -> Arc<Block> {
    let g = Arc::new(Block::new(eth_block(0, 0, B256::ZERO), None, None).expect("genesis"));
    g.mark_synchronous((
        ava_vm::components::gas::Gas(0),
        ava_saevm_gastime::GasPriceConfig::default(),
    ))
    .expect("mark synchronous");
    g
}

fn mark_executed_at(block: &Arc<Block>, exec_unix: u64) {
    let results = ExecutionResults {
        gas_time: Time::<u64>::new(exec_unix, 0, 1),
        base_fee: Price(1),
        receipt_root: B256::ZERO,
        post_state_root: B256::repeat_byte(0x33),
    };
    let artefacts = ExecutionArtefacts {
        interim_execution_time: results.gas_time.clone(),
        results,
    };
    block.mark_executed(artefacts, None).expect("mark executed");
}

fn build_chain(count: u64, last_settled_at: &[u64]) -> Vec<Arc<Block>> {
    let mut chain: Vec<Arc<Block>> = Vec::new();
    for height in 0..count {
        if height == 0 {
            chain.push(genesis());
            continue;
        }
        let parent = Arc::clone(&chain[(height - 1) as usize]);
        let last_settled = Arc::clone(&chain[last_settled_at[height as usize] as usize]);
        let eth = eth_block(height, height, parent.hash());
        let b = Arc::new(Block::new(eth, Some(parent), Some(last_settled)).expect("block"));
        chain.push(b);
    }
    chain
}

// ---------------------------------------------------------------------------
// Gather helpers.
// ---------------------------------------------------------------------------

/// The single-metric gauge value of family `name`, or `None` if absent.
fn gauge(families: &[MetricFamily], name: &str) -> Option<i64> {
    families
        .iter()
        .find(|f| f.get_name() == name)
        .and_then(|f| f.get_metric().first())
        .map(|m| m.get_gauge().get_value() as i64)
}

// ---------------------------------------------------------------------------
// (1) Registration exposes the three `sae` metric families with bare names.
// ---------------------------------------------------------------------------

#[test]
fn sae_metrics_register_exposes_three_families() {
    let frontier = Arc::new(Frontier::new(genesis()));
    let registry = Registry::new();
    SaeMetrics::new(Arc::clone(&frontier))
        .register_into(&registry)
        .expect("register sae metrics");

    let families = registry.gather();
    let names: Vec<&str> = families.iter().map(MetricFamily::get_name).collect();
    for want in [
        "last_settled_height",
        "last_executed_height",
        "in_memory_blocks",
    ] {
        assert!(names.contains(&want), "sae metric family {want} exposed");
    }
    // The names are bare (no `sae_`/`avalanche_` prefix — the gatherer applies
    // the namespace at node assembly, M8).
    assert!(
        !names.iter().any(|n| n.starts_with("sae_")),
        "metric names carry no namespace prefix; got {names:?}",
    );
}

// ---------------------------------------------------------------------------
// (2) The height gauges sample the live S/E frontiers at scrape time.
// ---------------------------------------------------------------------------

#[test]
fn sae_height_gauges_sample_frontier_at_scrape() {
    let chain = build_chain(6, &[0, 0, 0, 0, 0, 1]);
    let frontier = Arc::new(Frontier::new(Arc::clone(&chain[0])));
    let registry = Registry::new();
    SaeMetrics::new(Arc::clone(&frontier))
        .register_into(&registry)
        .expect("register");

    // At genesis both heights read 0.
    let g0 = registry.gather();
    assert_eq!(gauge(&g0, "last_settled_height"), Some(0), "S starts at 0");
    assert_eq!(gauge(&g0, "last_executed_height"), Some(0), "E starts at 0");

    // Drive the frontier forward AFTER registration; the collector re-samples.
    for h in 1..6u64 {
        let b = &chain[h as usize];
        frontier.advance_accepted(b);
        mark_executed_at(b, h);
        frontier.advance_executed(b);
        let _ = settle(&frontier, b);
    }

    let g1 = registry.gather();
    assert_eq!(
        gauge(&g1, "last_executed_height"),
        Some(i64::try_from(frontier.last_executed_height()).unwrap()),
        "E gauge tracks the live frontier",
    );
    assert_eq!(
        gauge(&g1, "last_settled_height"),
        Some(i64::try_from(frontier.last_settled_height()).unwrap()),
        "S gauge tracks the live frontier",
    );
    // E has advanced to the tip.
    assert_eq!(gauge(&g1, "last_executed_height"), Some(5), "E at tip");
}

// ---------------------------------------------------------------------------
// (3) `in_memory_blocks` samples the live GC counter at scrape time.
// ---------------------------------------------------------------------------

#[test]
fn in_memory_blocks_gauge_tracks_live_counter() {
    let frontier = Arc::new(Frontier::new(genesis()));
    let registry = Registry::new();
    SaeMetrics::new(Arc::clone(&frontier))
        .register_into(&registry)
        .expect("register");

    // The gauge equals the live process-wide counter at scrape time. (The
    // counter is a global static touched by other live blocks in the process,
    // so we assert equality-with-the-live-reading, not an absolute value.)
    let families = registry.gather();
    assert_eq!(
        gauge(&families, "in_memory_blocks"),
        Some(in_memory_block_count()),
        "in_memory_blocks samples the live GC counter",
    );

    // Hold some extra blocks alive; the next scrape reflects the higher count.
    let before = gauge(&registry.gather(), "in_memory_blocks").expect("present");
    let held = build_chain(4, &[0, 0, 0, 0]);
    let after = gauge(&registry.gather(), "in_memory_blocks").expect("present");
    assert!(
        after >= before + 4,
        "holding 4 more blocks alive raises in_memory_blocks ({before} -> {after})",
    );
    drop(held);
}
