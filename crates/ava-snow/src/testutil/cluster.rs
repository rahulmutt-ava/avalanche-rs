// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! An in-memory cluster of test-VM consensus instances (M3.1 scaffolding).
//!
//! At M3.1 the per-node consensus is a **placeholder**: `step` ingests a vote
//! bag per node and `accepted_chain` reports the genesis-rooted chain accepted
//! so far via the shared [`AcceptanceOracle`]. The real `Topological` wiring
//! (one instance per node) is added at M3.5, when the safety proptest is
//! un-ignored. The public API (`new`/`add_block`/`step`/`accepted_chain`) is
//! fixed now so the proptest body compiles today.

use std::sync::Arc;

use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::test_block::TestBlock;
use super::test_vm::{AcceptanceOracle, TestVm};
use crate::snowball::Parameters;

/// A simulated cluster of `n` Snowman nodes sharing one acceptance oracle.
pub struct Cluster {
    /// One test VM per simulated node.
    nodes: Vec<TestVm>,
    /// Consensus parameters every node runs with.
    params: Parameters,
    /// Genesis (last-accepted-at-start) block id.
    genesis: Id,
    /// Shared acceptance oracle (safety check target).
    oracle: Arc<AcceptanceOracle>,
}

impl Cluster {
    /// Builds a cluster of `n` nodes rooted at a deterministic genesis block,
    /// each running `params`.
    #[must_use]
    pub fn new(n: usize, params: Parameters) -> Self {
        let oracle = AcceptanceOracle::new();
        let genesis = Id::EMPTY;
        let nodes = (0..n).map(|_| TestVm::new(Arc::clone(&oracle))).collect();
        Self {
            nodes,
            params,
            genesis,
            oracle,
        }
    }

    /// The genesis block id (the chain root).
    #[must_use]
    pub fn genesis(&self) -> Id {
        self.genesis
    }

    /// The consensus parameters this cluster runs with.
    #[must_use]
    pub fn params(&self) -> Parameters {
        self.params
    }

    /// The number of nodes in the cluster.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the cluster has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Registers `block` on every node (the block becomes processable).
    pub fn add_block(&mut self, block: TestBlock) {
        for node in &mut self.nodes {
            node.add_block(block.clone());
        }
    }

    /// Applies one round of voting: `votes[i]` is node `i`'s vote bag.
    ///
    /// At M3.1 this is a no-op placeholder beyond bounds-checking the input; at
    /// M3.5 each node will feed its bag into its `Topological` instance and any
    /// resulting acceptances will be recorded into the shared oracle. The
    /// signature is fixed now so the proptest harness compiles.
    pub fn step(&mut self, votes: &[Bag<Id>]) {
        debug_assert!(votes.len() <= self.nodes.len(), "more vote bags than nodes");
        // Placeholder: real per-node Topological::record_poll wiring lands at
        // M3.5. The oracle stays empty until then, so the safety proptest is
        // #[ignore]d (UN-IGNORE at M3.5).
        let _ = &self.oracle;
    }

    /// The accepted chain as a height-ordered `(height, id)` list, drawn from
    /// the shared oracle.
    #[must_use]
    pub fn accepted_chain(&self) -> Vec<(u64, Id)> {
        self.oracle.chain()
    }
}
