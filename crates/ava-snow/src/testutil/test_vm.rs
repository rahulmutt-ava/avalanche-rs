// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! An in-memory test VM + shared acceptance oracle for the consensus cluster.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ava_types::id::Id;

use crate::decidable::Block;

use super::test_block::TestBlock;

/// Records the linear sequence of blocks a node has accepted. Shared across the
/// cluster so the safety property can assert no two conflicting blocks (same
/// height, different id) are ever both accepted.
#[derive(Debug, Default)]
pub struct AcceptanceOracle {
    /// Accepted block id at each height (genesis-rooted chain). A `BTreeMap`
    /// keeps height-ordered iteration deterministic (specs 00 §6.1).
    accepted: Mutex<BTreeMap<u64, Id>>,
}

impl AcceptanceOracle {
    /// A fresh oracle with no acceptances recorded.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records that `id` was accepted at `height`. Returns `Err` with the
    /// previously-accepted id if a *different* block was already accepted at
    /// this height (a safety violation the proptest guards against).
    ///
    /// # Errors
    /// Returns the conflicting id when `height` already holds a different id.
    pub fn record(&self, height: u64, id: Id) -> Result<(), Id> {
        let mut accepted = self
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match accepted.get(&height) {
            Some(&prev) if prev != id => Err(prev),
            _ => {
                accepted.insert(height, id);
                Ok(())
            }
        }
    }

    /// The accepted chain as a height-ordered `(height, id)` list.
    #[must_use]
    pub fn chain(&self) -> Vec<(u64, Id)> {
        let accepted = self
            .accepted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        accepted.iter().map(|(&h, &id)| (h, id)).collect()
    }
}

/// A no-op in-memory VM: it owns a set of known blocks and an
/// [`AcceptanceOracle`] handle. The cluster drives one `TestVm` per simulated
/// node. The consensus wiring is added at M3.5.
#[derive(Debug)]
pub struct TestVm {
    /// Blocks this VM knows about, keyed by id.
    blocks: BTreeMap<Id, TestBlock>,
    /// The shared acceptance oracle for this cluster.
    oracle: Arc<AcceptanceOracle>,
}

impl TestVm {
    /// Builds a test VM sharing the given acceptance oracle.
    #[must_use]
    pub fn new(oracle: Arc<AcceptanceOracle>) -> Self {
        Self {
            blocks: BTreeMap::new(),
            oracle,
        }
    }

    /// Registers a block this VM knows about.
    pub fn add_block(&mut self, block: TestBlock) {
        self.blocks.insert(block.id(), block);
    }

    /// Looks up a known block by id.
    #[must_use]
    pub fn get_block(&self, id: &Id) -> Option<&TestBlock> {
        self.blocks.get(id)
    }

    /// The shared acceptance oracle.
    #[must_use]
    pub fn oracle(&self) -> &Arc<AcceptanceOracle> {
        &self.oracle
    }
}
