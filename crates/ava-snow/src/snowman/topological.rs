// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`Topological`]: the Snowman consensus implementation (specs 06 §2.4; Go
//! `snow/consensus/snowman/topological.go`, `snowman_block.go`).
//!
//! `Topological` tracks the strongly preferred branch with a tree of snowball
//! instances, amortizing each network poll across more than just the next
//! block. A Kahn topological sort pushes votes towards genesis; blocks reaching
//! an alpha majority record the poll on their children, all others falter.
//!
//! This is a transition-exact port of the Go implementation, including the
//! critical acceptance-ordering invariant (the [`BlockAcceptor`] fires **before**
//! the block's own `accept`) and the falter/preferred-branch-walk rules.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::block::{Block, BlockAcceptor, NoOpBlockAcceptor};
use super::consensus::SnowmanConsensus;
use crate::error::{Error, Result};
use crate::snowball::Parameters;
use crate::snowball::consensus::{Consensus, Factory};
use crate::snowball::tree::Tree;

/// The maximum average acceptance time before the chain reports unhealthy
/// (Go `maxAcceptanceTime`).
const MAX_ACCEPTANCE_TIME: Duration = Duration::from_secs(15);

/// Tracks the state of a single block in the [`Topological`] tree (Go
/// `snowmanBlock`).
struct SnowmanBlock<F: Factory> {
    /// The block this node contains. `None` for the last accepted block /
    /// genesis (treated as decided).
    blk: Option<Arc<dyn Block>>,
    /// Set when this node and all its descendants received fewer than alpha
    /// votes and must reset before the next positive vote.
    should_falter: bool,
    /// The snowball instance deciding this block's canonical child. `None` until
    /// a child is issued.
    sb: Option<Tree<F>>,
    /// The blocks naming this block as their parent. Empty until a child is
    /// issued.
    children: BTreeMap<Id, Arc<dyn Block>>,
}

impl<F: Factory + Clone> SnowmanBlock<F> {
    /// The genesis / last-accepted node (no block, decided).
    fn genesis() -> Self {
        Self {
            blk: None,
            should_falter: false,
            sb: None,
            children: BTreeMap::new(),
        }
    }

    /// A processing node wrapping `blk`.
    fn processing(blk: Arc<dyn Block>) -> Self {
        Self {
            blk: Some(blk),
            should_falter: false,
            sb: None,
            children: BTreeMap::new(),
        }
    }

    /// Whether this node is decided (the genesis/last-accepted block).
    fn decided(&self, last_accepted_height: u64) -> bool {
        match &self.blk {
            None => true,
            Some(blk) => blk.height() <= last_accepted_height,
        }
    }

    /// Adds `child` as a child of this node, lazily creating the snowball tree
    /// on the first child (Go `snowmanBlock.AddChild`).
    fn add_child(&mut self, factory: &F, params: Parameters, child: Arc<dyn Block>) {
        let child_id = child.id();
        match &mut self.sb {
            None => {
                self.sb = Some(Tree::new(factory.clone(), params, child_id));
            }
            Some(sb) => sb.add(child_id),
        }
        self.children.insert(child_id, child);
    }
}

/// Tracks the Kahn topological-sort status of a block during vote propagation.
#[derive(Default)]
struct KahnNode {
    /// The number of unprocessed children. A node with `in_degree == 0` is a
    /// leaf.
    in_degree: i64,
    /// Accumulated votes for this node's children.
    votes: Bag<Id>,
}

/// A `(parent_id, votes)` entry on the vote stack.
struct Votes {
    parent_id: Id,
    votes: Bag<Id>,
}

/// The Snowman consensus implementation (Go `Topological`). Generic over the
/// snowball [`Factory`] that produces each block's child-decision instance.
pub struct Topological<F: Factory> {
    /// Produces snowball instances for each block's children.
    factory: F,
    /// The number of times `record_poll` has been called.
    poll_number: u64,
    /// The block-accept callback (fires before each block's own `accept`).
    block_acceptor: Arc<dyn BlockAcceptor>,
    /// Snowball parameters for every instance.
    params: Parameters,
    /// The last accepted block id.
    last_accepted_id: Id,
    /// The last accepted block height.
    last_accepted_height: u64,
    /// The last accepted block and all pending blocks, keyed by id.
    blocks: BTreeMap<Id, SnowmanBlock<F>>,
    /// The currently preferred block ids.
    preferred_ids: BTreeSet<Id>,
    /// The preferred block id at each height.
    preferred_heights: BTreeMap<u64, Id>,
    /// The preferred block with the highest height (the tail).
    preference: Id,
}

impl<F: Factory + Clone> Topological<F> {
    /// Builds and initializes a `Topological` rooted at `last_accepted_id`
    /// (folds Go `TopologicalFactory.New` + `Initialize`).
    ///
    /// # Errors
    /// Returns [`Error::ParametersInvalid`] if `params` fail validation.
    pub fn new(
        factory: F,
        block_acceptor: Arc<dyn BlockAcceptor>,
        params: Parameters,
        last_accepted_id: Id,
        last_accepted_height: u64,
    ) -> Result<Self> {
        params.verify()?;
        let mut blocks = BTreeMap::new();
        blocks.insert(last_accepted_id, SnowmanBlock::genesis());
        Ok(Self {
            factory,
            poll_number: 0,
            block_acceptor,
            params,
            last_accepted_id,
            last_accepted_height,
            blocks,
            preferred_ids: BTreeSet::new(),
            preferred_heights: BTreeMap::new(),
            preference: last_accepted_id,
        })
    }

    /// Builds a `Topological` with a no-op block acceptor (tests).
    ///
    /// # Errors
    /// Returns [`Error::ParametersInvalid`] if `params` fail validation.
    pub fn new_default(
        factory: F,
        params: Parameters,
        last_accepted_id: Id,
        last_accepted_height: u64,
    ) -> Result<Self> {
        Self::new(
            factory,
            Arc::new(NoOpBlockAcceptor),
            params,
            last_accepted_id,
            last_accepted_height,
        )
    }

    /// Health-check JSON shape + threshold errors (Go `HealthCheck`).
    ///
    /// Returns a `serde_json::Value` object mirroring Go's map, alongside an
    /// optional joined error when any health threshold is exceeded. The
    /// processing-time / average-acceptance-time inputs are passed in because
    /// the metrics subsystem they derive from in Go is not yet ported; tests
    /// drive them directly.
    ///
    /// # Errors
    /// Returns [`Error::Multiple`] joining any of
    /// [`Error::TooManyProcessingBlocks`], [`Error::BlockProcessingTooLong`],
    /// [`Error::AcceptanceTimeTooHigh`] that tripped.
    pub fn health_check(
        &self,
        longest_processing: Duration,
        avg_acceptance_time: Duration,
    ) -> (serde_json::Value, Result<()>) {
        let mut errs = Vec::new();

        let num_processing = self.num_processing();
        if num_processing > self.params.max_outstanding_items as usize {
            errs.push(Error::TooManyProcessingBlocks);
        }
        if longest_processing > self.params.max_item_processing_time {
            errs.push(Error::BlockProcessingTooLong);
        }
        if avg_acceptance_time > MAX_ACCEPTANCE_TIME {
            errs.push(Error::AcceptanceTimeTooHigh);
        }

        let details = serde_json::json!({
            "processingBlocks": num_processing,
            "longestProcessingBlock": format!("{longest_processing:?}"),
            "avgAcceptanceTime": format!("{avg_acceptance_time:?}"),
            "lastAcceptedID": self.last_accepted_id.to_string(),
            "lastAcceptedHeight": self.last_accepted_height,
        });

        let result = if errs.is_empty() {
            Ok(())
        } else {
            Err(Error::Multiple(errs))
        };
        (details, result)
    }

    // ----- vote propagation (Go calculateInDegree / pushVotes / vote) -----

    /// Sets up the topological ordering of the blocks reachable from `votes`,
    /// returning the Kahn-annotated node map and the initial leaf set (Go
    /// `calculateInDegree`).
    fn calculate_in_degree(&self, votes: &Bag<Id>) -> (BTreeMap<Id, KahnNode>, BTreeSet<Id>) {
        let mut kahn_nodes: BTreeMap<Id, KahnNode> = BTreeMap::new();
        let mut leaves: BTreeSet<Id> = BTreeSet::new();

        for vote in votes.list() {
            let Some(voted_block) = self.blocks.get(&vote) else {
                continue; // Vote for a block not in the pending set: dropped.
            };
            if voted_block.decided(self.last_accepted_height) {
                continue; // Vote for the last accepted block: dropped.
            }

            // The parent holds the snowball instance of its children.
            let Some(blk) = &voted_block.blk else {
                continue;
            };
            let mut parent_id = blk.parent();

            let num_votes = votes.count(&vote);
            let previously_seen = kahn_nodes.contains_key(&parent_id);
            let kahn = kahn_nodes.entry(parent_id).or_default();
            kahn.votes.add_count(vote, num_votes);

            if previously_seen {
                continue;
            }

            // First time we've seen this parent: currently a leaf.
            leaves.insert(parent_id);

            // Walk ancestors, setting up in-degrees.
            while let Some(n) = self.blocks.get(&parent_id) {
                if n.decided(self.last_accepted_height) {
                    break;
                }
                let Some(n_blk) = &n.blk else {
                    break;
                };
                parent_id = n_blk.parent();

                let previously_seen = kahn_nodes.contains_key(&parent_id);
                let kahn = kahn_nodes.entry(parent_id).or_default();
                kahn.in_degree = kahn.in_degree.saturating_add(1);

                if previously_seen {
                    // Don't re-increment ancestors through this block.
                    leaves.remove(&parent_id);
                    break;
                }
            }
        }
        (kahn_nodes, leaves)
    }

    /// Converts the Kahn graph into a branch of snowball instances with at least
    /// alpha votes (Go `pushVotes`). Consumes `kahn_nodes`/`leaves`.
    fn push_votes(
        &self,
        mut kahn_nodes: BTreeMap<Id, KahnNode>,
        mut leaves: BTreeSet<Id>,
    ) -> Vec<Votes> {
        let mut vote_stack: Vec<Votes> = Vec::with_capacity(kahn_nodes.len());
        while let Some(&leaf_id) = leaves.iter().next() {
            leaves.remove(&leaf_id);

            let kahn_votes_len = kahn_nodes.get(&leaf_id).map_or(0, |k| k.votes.len());

            if kahn_votes_len >= self.params.alpha_preference as usize {
                // Move the leaf's votes onto the stack.
                if let Some(kahn) = kahn_nodes.get_mut(&leaf_id) {
                    let votes = std::mem::replace(&mut kahn.votes, Bag::new());
                    vote_stack.push(Votes {
                        parent_id: leaf_id,
                        votes,
                    });
                }
            }

            let Some(block) = self.blocks.get(&leaf_id) else {
                continue;
            };
            if block.decided(self.last_accepted_height) {
                continue; // No need to push votes past an accepted block.
            }
            let Some(blk) = &block.blk else {
                continue;
            };
            let parent_id = blk.parent();

            let parent_kahn = kahn_nodes.entry(parent_id).or_default();
            parent_kahn.in_degree = parent_kahn.in_degree.saturating_sub(1);
            parent_kahn.votes.add_count(leaf_id, kahn_votes_len);
            if parent_kahn.in_degree == 0 {
                leaves.insert(parent_id);
            }
        }
        vote_stack
    }

    /// Applies the votes on the alpha-threshold branch, accepting/rejecting as
    /// needed, and returns the next preferred block after the last
    /// alpha-threshold block (Go `vote`).
    fn vote(&mut self, mut vote_stack: Vec<Votes>) -> Result<Id> {
        if vote_stack.is_empty() {
            // The full tree should falter.
            if let Some(last) = self.blocks.get_mut(&self.last_accepted_id) {
                last.should_falter = true;
            }
            return Ok(self.preference);
        }

        let mut new_preferred = self.last_accepted_id;
        let mut on_preferred_branch = true;

        while let Some(vote) = vote_stack.pop() {
            // The block we are about to vote on; if rejected/removed, stop.
            if !self.blocks.contains_key(&vote.parent_id) {
                break;
            }

            let should_transitively_falter = self
                .blocks
                .get(&vote.parent_id)
                .is_some_and(|b| b.should_falter);

            if should_transitively_falter && let Some(b) = self.blocks.get_mut(&vote.parent_id) {
                if let Some(sb) = &mut b.sb {
                    sb.record_unsuccessful_poll();
                }
                b.should_falter = false;
            }

            // Apply the votes to this snowball instance.
            let (finalized, parent_preference) = {
                let Some(b) = self.blocks.get_mut(&vote.parent_id) else {
                    break;
                };
                if let Some(sb) = &mut b.sb {
                    sb.record_poll(&vote.votes);
                    (sb.finalized(), sb.preference())
                } else {
                    (false, Id::EMPTY)
                }
            };

            // Accept only when finalized AND a child of the last accepted block.
            if finalized && self.last_accepted_id == vote.parent_id {
                self.accept_preferred_child(&vote.parent_id)?;
                // The last accepted block is now the accepted child; remove the
                // old parent node.
                self.blocks.remove(&vote.parent_id);
            }

            if on_preferred_branch {
                new_preferred = parent_preference;
            }

            // The next child to receive a poll (else nil).
            let next_id = vote_stack.last().map_or(Id::EMPTY, |v| v.parent_id);

            on_preferred_branch = on_preferred_branch && next_id == parent_preference;

            // Falter every child except the one about to receive a poll.
            let child_ids: Vec<Id> = self
                .blocks
                .get(&vote.parent_id)
                .map(|b| b.children.keys().copied().collect())
                .unwrap_or_default();
            for child_id in child_ids {
                if !should_transitively_falter && child_id == next_id {
                    continue;
                }
                if let Some(child_block) = self.blocks.get_mut(&child_id) {
                    child_block.should_falter = true;
                }
            }
        }

        Ok(new_preferred)
    }

    /// Accepts the preferred child of `parent_id`, rejecting all siblings and
    /// their descendants (Go `acceptPreferredChild`).
    ///
    /// CRITICAL ORDERING: the block acceptor fires before the child's own
    /// `accept`.
    fn accept_preferred_child(&mut self, parent_id: &Id) -> Result<()> {
        // (preferred id, preferred child, siblings to reject).
        type PreferredAndRejects = (Id, Arc<dyn Block>, Vec<(Id, Arc<dyn Block>)>);
        // Determine the preferred child and gather sibling ids.
        let (pref, pref_child, reject_children): PreferredAndRejects = {
            let n = self
                .blocks
                .get(parent_id)
                .ok_or(Error::UnknownParentBlock)?;
            let pref = n.sb.as_ref().map_or(Id::EMPTY, Consensus::preference);
            let pref_child = n
                .children
                .get(&pref)
                .cloned()
                .ok_or(Error::UnknownParentBlock)?;
            let rejects = n
                .children
                .iter()
                .filter(|(id, _)| **id != pref)
                .map(|(id, blk)| (*id, blk.clone()))
                .collect();
            (pref, pref_child, rejects)
        };

        // Acceptor BEFORE the block's own accept (invariant).
        let bytes = pref_child.bytes().to_vec();
        self.block_acceptor.accept(pref, &bytes)?;

        let height = pref_child.height();
        pref_child.accept()?;

        self.last_accepted_id = pref;
        self.last_accepted_height = height;
        self.preferred_ids.remove(&pref);
        self.preferred_heights.remove(&height);

        // Reject the siblings, then their descendants.
        let mut rejects = Vec::with_capacity(reject_children.len());
        for (child_id, child) in reject_children {
            child.reject()?;
            rejects.push(child_id);
        }
        self.reject_transitively(rejects)
    }

    /// Rejects all descendants of the already-rejected `rejected` ids (Go
    /// `rejectTransitively`).
    fn reject_transitively(&mut self, mut rejected: Vec<Id>) -> Result<()> {
        while let Some(rejected_id) = rejected.pop() {
            let Some(rejected_node) = self.blocks.remove(&rejected_id) else {
                continue;
            };
            for (child_id, child) in rejected_node.children {
                child.reject()?;
                rejected.push(child_id);
            }
        }
        Ok(())
    }
}

impl<F: Factory + Clone> SnowmanConsensus for Topological<F> {
    fn num_processing(&self) -> usize {
        self.blocks.len().saturating_sub(1)
    }

    fn add(&mut self, block: Arc<dyn Block>) -> Result<()> {
        let blk_id = block.id();
        let height = block.height();
        let parent_id = block.parent();

        if self.processing(blk_id) {
            return Err(Error::DuplicateAdd);
        }
        if !self.blocks.contains_key(&parent_id) {
            return Err(Error::UnknownParentBlock);
        }

        // Add the block as a child of its parent.
        let factory = self.factory.clone();
        let params = self.params;
        if let Some(parent_node) = self.blocks.get_mut(&parent_id) {
            parent_node.add_child(&factory, params, block.clone());
        }
        self.blocks.insert(blk_id, SnowmanBlock::processing(block));

        // Extending the preference makes this the new preference.
        if self.preference == parent_id {
            self.preference = blk_id;
            self.preferred_ids.insert(blk_id);
            self.preferred_heights.insert(height, blk_id);
        }
        Ok(())
    }

    fn processing(&self, block_id: Id) -> bool {
        if block_id == self.last_accepted_id {
            return false;
        }
        self.blocks.contains_key(&block_id)
    }

    fn is_preferred(&self, block_id: Id) -> bool {
        block_id == self.last_accepted_id || self.preferred_ids.contains(&block_id)
    }

    fn last_accepted(&self) -> (Id, u64) {
        (self.last_accepted_id, self.last_accepted_height)
    }

    fn preference(&self) -> Id {
        self.preference
    }

    fn preference_at_height(&self, height: u64) -> Option<Id> {
        if height == self.last_accepted_height {
            return Some(self.last_accepted_id);
        }
        self.preferred_heights.get(&height).copied()
    }

    fn record_poll(&mut self, vote_bag: &Bag<Id>) -> Result<()> {
        self.poll_number = self.poll_number.saturating_add(1);

        let mut vote_stack = Vec::new();
        if vote_bag.len() >= self.params.alpha_preference as usize {
            let (kahn_nodes, leaves) = self.calculate_in_degree(vote_bag);
            vote_stack = self.push_votes(kahn_nodes, leaves);
        }

        let preferred = self.vote(vote_stack)?;

        if self.preferred_ids.contains(&preferred) {
            return Ok(());
        }

        self.preferred_ids.clear();
        self.preferred_heights.clear();

        self.preference = preferred;

        // Walk from the preferred id down to the last accepted ancestor.
        let mut cursor = self.preference;
        while let Some(block) = self.blocks.get(&cursor) {
            if block.decided(self.last_accepted_height) {
                break;
            }
            let Some(blk) = &block.blk else {
                break;
            };
            let height = blk.height();
            let parent = blk.parent();
            self.preferred_ids.insert(cursor);
            self.preferred_heights.insert(height, cursor);
            cursor = parent;
        }

        // Walk from the preferred id to the preferred child until no children.
        let mut cursor = self.preference;
        while let Some(next) = self
            .blocks
            .get(&cursor)
            .and_then(|block| block.sb.as_ref())
            .map(Consensus::preference)
        {
            self.preference = next;
            self.preferred_ids.insert(next);
            if let Some(blk) = self.blocks.get(&next).and_then(|child| child.blk.as_ref()) {
                self.preferred_heights.insert(blk.height(), next);
            }
            cursor = next;
        }
        Ok(())
    }

    fn get_parent(&self, id: Id) -> Option<Id> {
        self.blocks
            .get(&id)
            .and_then(|b| b.blk.as_ref())
            .map(|blk| blk.parent())
    }
}
