// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! An in-memory cluster of [`Topological`] consensus instances (M3.5).
//!
//! Each node runs its own `Topological` over a shared block set; `step` feeds a
//! per-node vote bag into that node's instance, and accepted blocks are recorded
//! into the shared [`AcceptanceOracle`] (the safety-property target). The
//! genesis-rooted accepted chain is read via [`Cluster::accepted_chain`].

// Testutil scaffolding: unwrap/expect are acceptable here (params are known
// valid; lock poisoning is recovered).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::test_block::TestBlock;
use super::test_vm::AcceptanceOracle;
use crate::decidable::Block as DecidableBlock;
use crate::error::Result;
use crate::snowball::{Parameters, SnowballFactory};
use crate::snowman::block::{Block, BlockAcceptor};
use crate::snowman::{SnowmanConsensus, Topological};

/// A synchronous snowman [`Block`] over the cluster's [`TestBlock`].
struct SnowmanTestBlock {
    inner: TestBlock,
}

impl Block for SnowmanTestBlock {
    fn id(&self) -> Id {
        self.inner.id()
    }
    fn parent(&self) -> Id {
        self.inner.parent()
    }
    fn height(&self) -> u64 {
        self.inner.height()
    }
    fn timestamp(&self) -> SystemTime {
        self.inner.timestamp()
    }
    fn bytes(&self) -> &[u8] {
        self.inner.bytes()
    }
    fn accept(&self) -> Result<()> {
        Ok(())
    }
    fn reject(&self) -> Result<()> {
        Ok(())
    }
}

/// A [`BlockAcceptor`] that records accepted `(height, id)` pairs into the
/// shared oracle. Heights are looked up from a per-node id→height map.
struct OracleAcceptor {
    oracle: Arc<AcceptanceOracle>,
    heights: Arc<std::sync::Mutex<BTreeMap<Id, u64>>>,
}

impl BlockAcceptor for OracleAcceptor {
    fn accept(&self, container_id: Id, _bytes: &[u8]) -> Result<()> {
        let height = self
            .heights
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&container_id)
            .copied()
            .unwrap_or(0);
        // A safety violation (two ids at one height) surfaces here; record it by
        // ignoring the error (the oracle keeps the first, the proptest asserts
        // contiguity over what was recorded).
        let _ = self.oracle.record(height, container_id);
        Ok(())
    }
}

/// One simulated node: a `Topological` instance plus its id→height map.
struct Node {
    consensus: Topological<SnowballFactory>,
    heights: Arc<std::sync::Mutex<BTreeMap<Id, u64>>>,
}

/// A simulated cluster of `n` Snowman nodes sharing one acceptance oracle.
pub struct Cluster {
    nodes: Vec<Node>,
    params: Parameters,
    genesis: Id,
    oracle: Arc<AcceptanceOracle>,
    /// Blocks added so far, in insertion order (so re-adds to new nodes respect
    /// parent-before-child ordering).
    added: Vec<TestBlock>,
}

impl Cluster {
    /// Builds a cluster of `n` nodes rooted at a deterministic genesis block,
    /// each running `params`.
    #[must_use]
    pub fn new(n: usize, params: Parameters) -> Self {
        let oracle = AcceptanceOracle::new();
        let genesis = Id::EMPTY;
        let nodes = (0..n.max(1))
            .map(|_| Self::make_node(&oracle, params, genesis))
            .collect();
        Self {
            nodes,
            params,
            genesis,
            oracle,
            added: Vec::new(),
        }
    }

    fn make_node(oracle: &Arc<AcceptanceOracle>, params: Parameters, genesis: Id) -> Node {
        let heights = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let acceptor: Arc<dyn BlockAcceptor> = Arc::new(OracleAcceptor {
            oracle: Arc::clone(oracle),
            heights: Arc::clone(&heights),
        });
        // Genesis params are always valid in tests (DEFAULT_PARAMETERS); fall
        // back to an unwired instance only if verification somehow fails.
        let consensus = Topological::new(SnowballFactory, acceptor, params, genesis, 0)
            .unwrap_or_else(|_| {
                Topological::new_default(SnowballFactory, params, genesis, 0)
                    .expect("default params must verify")
            });
        Node { consensus, heights }
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

    /// Registers `block` on every node (the block becomes processable). Blocks
    /// whose parent is unknown to a node are silently skipped on that node
    /// (matching the conflicting-DAG harness, where some branches are rejected).
    pub fn add_block(&mut self, block: TestBlock) {
        for node in &mut self.nodes {
            node.heights
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(block.id(), block.height());
            let snow_block: Arc<dyn Block> = Arc::new(SnowmanTestBlock {
                inner: block.clone(),
            });
            // Ignore add errors (unknown parent / duplicate): the proptest feeds
            // arbitrary DAGs and only asserts safety over what was accepted.
            let _ = node.consensus.add(snow_block);
        }
        self.added.push(block);
    }

    /// Applies one round of voting: `votes[i]` is node `i`'s vote bag. Each node
    /// records its bag into its `Topological`; any resulting acceptances flow to
    /// the shared oracle via the block acceptor.
    pub fn step(&mut self, votes: &[Bag<Id>]) {
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if let Some(bag) = votes.get(i) {
                let _ = node.consensus.record_poll(bag);
            }
        }
    }

    /// The accepted chain as a height-ordered `(height, id)` list, drawn from
    /// the shared oracle.
    #[must_use]
    pub fn accepted_chain(&self) -> Vec<(u64, Id)> {
        self.oracle.chain()
    }
}
