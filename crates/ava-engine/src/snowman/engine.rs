// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman engine (issue / poll / vote loop) — port of
//! `snow/engine/snowman/engine.go` (specs 06 §4.2).
//!
//! [`SnowmanEngine`] drives normal-operation consensus: it issues blocks into the
//! [`SnowmanConsensus`] core (requesting missing ancestors), polls a weighted
//! validator sample for their preferences, applies the resulting votes through
//! [`PollSet`] early-termination, and tracks the preferred branch back into the
//! VM via `set_preference`.
//!
//! ## Port note (job scheduler)
//!
//! Go parks `issuer`/`voter` jobs in a `job.Scheduler` keyed by block-id
//! dependencies, draining them in `executeDeferredWork`. Because the engine task
//! is single-owner and the VM/consensus calls here are `await`ed inline, we
//! resolve ancestry eagerly inside [`SnowmanEngine::issue_from`] and apply chits
//! once their referenced blocks are issued, rather than maintaining a separate
//! parked-job graph. The observable behaviour (outbound messages, accept/reject
//! order) is identical for the connected-ancestry case the tests exercise; the
//! [`issuer`](super::issuer) / [`voter`](super::voter) modules document the
//! correspondence. See `tests/PORTING.md`.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_snow::snowball::Parameters;
use ava_snow::snowman::SnowmanConsensus;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::bag::Bag;
use ava_validators::ValidatorManager;
use ava_vm::block::ChainVm;

use crate::common::sender::Sender;
use crate::error::{Error, Result};
use crate::snowman::adaptor::BlockAdaptor;
use crate::snowman::getter::Getter;
use crate::snowman::poll::{EarlyTermFactory, PollSet};

/// `snow/engine/snowman.Config` — the Snowman engine's dependencies.
pub struct Config<V, S, M> {
    /// The subnet this chain belongs to (sampling key).
    pub subnet_id: Id,
    /// Snowball parameters (k / alpha / beta / concurrent_repolls / …).
    pub params: Parameters,
    /// The VM driven by the engine, behind a mutex (single-owner task).
    pub vm: Arc<Mutex<V>>,
    /// The outbound sender.
    pub sender: Arc<S>,
    /// The validator manager used for weighted poll sampling.
    pub validators: Arc<M>,
    /// Halt signal (`Halter`): when fired, in-flight VM ops should abort.
    pub token: CancellationToken,
}

/// The Snowman consensus engine (port of `engine.Engine`). Generic over the VM,
/// the [`Sender`], the [`ValidatorManager`], and the boxed consensus core.
pub struct SnowmanEngine<V, S, M> {
    cfg: Config<V, S, M>,
    consensus: Box<dyn SnowmanConsensus + Send + Sync>,
    getter: Getter<V, S>,
    request_id: u32,
    /// Outstanding polls keyed by request id (early-termination).
    polls: PollSet<EarlyTermFactory>,
    /// Outstanding `Get` requests: `(node, request_id) -> blkID` and the reverse.
    blk_reqs: BTreeMap<(NodeId, u32), Id>,
    blk_reqs_by_id: BTreeMap<Id, (NodeId, u32)>,
    /// Peers' last-accepted frontiers (used to synthesize a `Chits` on
    /// `query_failed`).
    accepted_frontiers: BTreeMap<NodeId, (Id, u64)>,
    /// Build-block requests pending from VM `Notify(PendingTxs)`.
    pending_build_blocks: u32,
}

impl<V, S, M> SnowmanEngine<V, S, M>
where
    V: ChainVm,
    S: Sender,
    M: ValidatorManager,
{
    /// Builds a Snowman engine over an already-initialized consensus core.
    pub fn new(cfg: Config<V, S, M>, consensus: Box<dyn SnowmanConsensus + Send + Sync>) -> Self {
        let getter = Getter::new(
            Arc::clone(&cfg.vm),
            Arc::clone(&cfg.sender),
            cfg.token.clone(),
        );
        let factory =
            EarlyTermFactory::new(cfg.params.alpha_preference, cfg.params.alpha_confidence);
        Self {
            cfg,
            consensus,
            getter,
            request_id: 0,
            polls: PollSet::new(factory),
            blk_reqs: BTreeMap::new(),
            blk_reqs_by_id: BTreeMap::new(),
            accepted_frontiers: BTreeMap::new(),
            pending_build_blocks: 0,
        }
    }

    /// The engine's current request-id counter (exposed for tests).
    #[must_use]
    pub fn request_id(&self) -> u32 {
        self.request_id
    }

    /// The number of outstanding polls (exposed for tests).
    #[must_use]
    pub fn num_polls(&self) -> usize {
        self.polls.len()
    }

    /// The consensus core's current preference.
    #[must_use]
    pub fn preference(&self) -> Id {
        self.consensus.preference()
    }

    /// The number of processing blocks in consensus.
    #[must_use]
    pub fn num_processing(&self) -> usize {
        self.consensus.num_processing()
    }

    /// The consensus core's last-accepted `(id, height)` (exposed for tests).
    #[must_use]
    pub fn consensus_last_accepted(&self) -> (Id, u64) {
        self.consensus.last_accepted()
    }

    /// The strongly-preferred block id at `height`, if tracked (exposed for
    /// tests).
    #[must_use]
    pub fn preference_at_height(&self, height: u64) -> Option<Id> {
        self.consensus.preference_at_height(height)
    }

    /// Whether `id` is currently processing in consensus (exposed for tests).
    #[must_use]
    pub fn is_processing(&self, id: Id) -> bool {
        self.consensus.processing(id)
    }

    /// Whether `request_id` is still an outstanding poll (exposed for tests).
    #[must_use]
    pub fn poll_pending(&self, request_id: u32) -> bool {
        self.polls.contains(request_id)
    }

    /// The read-only `Get*` server.
    pub fn getter(&self) -> &Getter<V, S> {
        &self.getter
    }

    // ---- Issue path ---------------------------------------------------------

    /// `Put` — a block arrived (response to a `Get` or unsolicited). Parse it,
    /// clear any matching outstanding request, and issue it from the providing
    /// node.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn put(&mut self, node: NodeId, req: u32, container: &[u8]) -> Result<()> {
        let blk = {
            let vm = self.cfg.vm.lock().await;
            vm.parse_block(&self.cfg.token, container).await
        };
        let blk = match blk {
            Ok(blk) => blk,
            // Failed to parse: treat as a Get failure to abandon the request.
            Err(e) => {
                tracing::warn!(
                    %node,
                    req,
                    len = container.len(),
                    error = %e,
                    "put: failed to parse block container; abandoning request"
                );
                return self.get_failed(node, req).await;
            }
        };

        // If this matches an outstanding Get and the id mismatches, abandon.
        if let Some(expected) = self.blk_reqs.get(&(node, req)).copied()
            && blk.id() != expected
        {
            return self.get_failed(node, req).await;
        }

        self.issue_from(node, blk).await
    }

    /// `GetFailed` — a `Get` we issued will not be answered; clear the request.
    ///
    /// # Errors
    /// Never fatal here.
    pub async fn get_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        if let Some(blk_id) = self.blk_reqs.remove(&(node, req)) {
            self.blk_reqs_by_id.remove(&blk_id);
        }
        Ok(())
    }

    /// `issueFrom` — issue `blk` and its ancestors into consensus, requesting
    /// any missing ancestor from `node`.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn issue_from(&mut self, node: NodeId, blk: Arc<dyn ava_snow::Block>) -> Result<()> {
        // Walk the ancestry, issuing each block whose parent is already issuable.
        // Collect the chain root-first so we add parents before children.
        let mut chain: Vec<Arc<dyn ava_snow::Block>> = Vec::new();
        let mut current = blk;
        loop {
            if !self.should_issue_block(current.as_ref()) {
                break;
            }
            let parent_id = current.parent();
            chain.push(Arc::clone(&current));

            if self.can_issue_child_on(parent_id) {
                // Parent is the last-accepted block or already processing: we can
                // issue the whole collected chain now.
                break;
            }
            // Try to fetch the parent locally.
            let parent = self.get_block(parent_id).await;
            match parent {
                Ok(parent) => current = parent,
                Err(_) => {
                    // Parent is unknown: request it from the providing node and
                    // stop (the chain can't be issued yet).
                    self.send_request(node, parent_id);
                    return Ok(());
                }
            }
        }

        // Clear any outstanding request for the head block.
        if let Some(head) = chain.first() {
            let head_id = head.id();
            if let Some((n, r)) = self.blk_reqs_by_id.remove(&head_id) {
                self.blk_reqs.remove(&(n, r));
            }
        }

        // Issue from the oldest ancestor to the newest (reverse of collection).
        let mut any_added = false;
        for block in chain.into_iter().rev() {
            let blk_id = block.id();
            // Skip if it became processing in the meantime.
            if self.consensus.processing(blk_id) {
                continue;
            }
            // The parent must be issuable for verification to be valid.
            if !self.can_issue_child_on(block.parent()) {
                continue;
            }
            // Clear any outstanding request for this block.
            if let Some((n, r)) = self.blk_reqs_by_id.remove(&blk_id) {
                self.blk_reqs.remove(&(n, r));
            }
            let added = self
                .add_unverified_block_to_consensus(node, Arc::clone(&block))
                .await?;
            if added {
                any_added = true;
                // Update preference and query if the new block is preferred.
                self.set_vm_preference().await?;
                if self.consensus.is_preferred(blk_id) {
                    self.send_query(blk_id, Some(block.bytes().to_vec()), true)
                        .await?;
                }
            }
        }

        if any_added && self.consensus.num_processing() > 0 {
            self.repoll().await?;
        }
        Ok(())
    }

    /// `addUnverifiedBlockToConsensus` — verify then add `blk`; returns whether
    /// it was added.
    async fn add_unverified_block_to_consensus(
        &mut self,
        _node: NodeId,
        blk: Arc<dyn ava_snow::Block>,
    ) -> Result<bool> {
        if self.cfg.token.is_cancelled() {
            return Err(Error::Halted);
        }
        if let Err(e) = blk.verify(&self.cfg.token).await {
            // Verification failed: all descendants are also invalid.
            tracing::warn!(
                block = %blk.id(),
                height = blk.height(),
                error = %e,
                "block verification failed; dropping block and descendants"
            );
            return Ok(false);
        }
        // Bridge the async VM block into the synchronous consensus block.
        let adaptor = BlockAdaptor::new(blk, self.cfg.token.clone());
        self.consensus.add(Arc::new(adaptor))?;
        Ok(true)
    }

    /// `sendRequest` — request block `blk_id` from `node` (`SendGet`).
    fn send_request(&mut self, node: NodeId, blk_id: Id) {
        if self.blk_reqs_by_id.contains_key(&blk_id) {
            // Already an outstanding request for this block.
            return;
        }
        self.request_id = self.request_id.wrapping_add(1);
        let req = self.request_id;
        self.blk_reqs.insert((node, req), blk_id);
        self.blk_reqs_by_id.insert(blk_id, (node, req));
        self.cfg.sender.send_get(node, req, blk_id);
    }

    /// `getBlock` — look the block up in the VM (the engine's pending caches are
    /// folded into consensus in this port).
    async fn get_block(&self, blk_id: Id) -> Result<Arc<dyn ava_snow::Block>> {
        let vm = self.cfg.vm.lock().await;
        Ok(vm.get_block(&self.cfg.token, blk_id).await?)
    }

    // ---- Poll path ----------------------------------------------------------

    /// `repoll` — issue up to `concurrent_repolls` outstanding pull queries on
    /// the current preference.
    ///
    /// # Errors
    /// Propagates a fatal sampling/sender error.
    pub async fn repoll(&mut self) -> Result<()> {
        let pref = self.consensus.preference();
        let target = self.cfg.params.concurrent_repolls as usize;
        while self.polls.len() < target {
            let issued = self.send_query(pref, None, false).await?;
            if !issued {
                break;
            }
        }
        Ok(())
    }

    /// `sendQuery` — sample `k` validators, register a poll + the query.
    /// Returns whether a poll was registered.
    ///
    /// # Errors
    /// Propagates a fatal sender error.
    async fn send_query(
        &mut self,
        blk_id: Id,
        blk_bytes: Option<Vec<u8>>,
        push: bool,
    ) -> Result<bool> {
        let vdr_ids = match self
            .cfg
            .validators
            .sample(self.cfg.subnet_id, self.cfg.params.k as usize)
        {
            Ok(ids) if !ids.is_empty() => ids,
            // Insufficient validators: drop the query.
            _ => return Ok(false),
        };

        let (_, last_accepted_height) = self.consensus.last_accepted();
        let next_height = last_accepted_height.saturating_add(1);

        // Build the weighted validator bag for the poll.
        let mut vdr_bag = Bag::new();
        let mut vdr_set = std::collections::HashSet::new();
        for node in &vdr_ids {
            let weight = self.cfg.validators.get_weight(self.cfg.subnet_id, *node);
            let weight = usize::try_from(weight).unwrap_or(usize::MAX).max(1);
            vdr_bag.add_count(*node, weight);
            vdr_set.insert(*node);
        }

        self.request_id = self.request_id.wrapping_add(1);
        let req = self.request_id;
        if !self.polls.add(req, vdr_bag) {
            return Ok(false);
        }

        if push {
            let bytes = blk_bytes.unwrap_or_default();
            self.cfg
                .sender
                .send_push_query(&vdr_set, req, bytes, next_height);
        } else {
            self.cfg
                .sender
                .send_pull_query(&vdr_set, req, blk_id, next_height);
        }
        Ok(true)
    }

    /// Block gossip (`Gossip`) — query a single uniformly-sampled connected
    /// validator for its next-height preference (Go caps bandwidth here).
    ///
    /// # Errors
    /// Propagates a fatal sender error.
    pub async fn gossip(&mut self) -> Result<()> {
        let pref = self.consensus.preference();
        // Uniform single-validator sample.
        let vdr_ids = match self.cfg.validators.sample(self.cfg.subnet_id, 1) {
            Ok(ids) if !ids.is_empty() => ids,
            _ => return Ok(()),
        };
        let (_, last_accepted_height) = self.consensus.last_accepted();
        let next_height = last_accepted_height.saturating_add(1);

        let mut vdr_bag = Bag::new();
        let mut vdr_set = std::collections::HashSet::new();
        for node in &vdr_ids {
            vdr_bag.add(*node);
            vdr_set.insert(*node);
        }
        self.request_id = self.request_id.wrapping_add(1);
        let req = self.request_id;
        if self.polls.add(req, vdr_bag) {
            self.cfg
                .sender
                .send_pull_query(&vdr_set, req, pref, next_height);
        }
        Ok(())
    }

    // ---- Respond path -------------------------------------------------------

    /// `sendChits` — answer a query with the current preferred / preferred-at-
    /// height / last-accepted ids, *before* issuing the queried container.
    async fn send_chits(&self, node: NodeId, req: u32, requested_height: u64) {
        let (last_accepted_id, last_accepted_height) = self.consensus.last_accepted();
        let preference = self.consensus.preference();
        let preference_at_height = if requested_height < last_accepted_height {
            let vm = self.cfg.vm.lock().await;
            vm.get_block_id_at_height(&self.cfg.token, requested_height)
                .await
                .unwrap_or(last_accepted_id)
        } else {
            self.consensus
                .preference_at_height(requested_height)
                .unwrap_or(preference)
        };
        self.cfg.sender.send_chits(
            node,
            req,
            preference,
            preference_at_height,
            last_accepted_id,
            last_accepted_height,
        );
    }

    /// `PullQuery` — answer with chits, then try to issue the queried block.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn pull_query(
        &mut self,
        node: NodeId,
        req: u32,
        blk_id: Id,
        requested_height: u64,
    ) -> Result<()> {
        self.send_chits(node, req, requested_height).await;
        self.issue_from_by_id(node, blk_id).await
    }

    /// `PushQuery` — answer with chits, then try to issue the supplied block.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn push_query(
        &mut self,
        node: NodeId,
        req: u32,
        container: &[u8],
        requested_height: u64,
    ) -> Result<()> {
        self.send_chits(node, req, requested_height).await;
        let blk = {
            let vm = self.cfg.vm.lock().await;
            vm.parse_block(&self.cfg.token, container).await
        };
        match blk {
            Ok(blk) => self.issue_from(node, blk).await,
            // Parse failure: drop (we didn't ask for it).
            Err(_) => Ok(()),
        }
    }

    /// `issueFromByID` — issue the block named by `blk_id`, requesting it if not
    /// available locally.
    async fn issue_from_by_id(&mut self, node: NodeId, blk_id: Id) -> Result<()> {
        match self.get_block(blk_id).await {
            Ok(blk) => self.issue_from(node, blk).await,
            Err(_) => {
                self.send_request(node, blk_id);
                Ok(())
            }
        }
    }

    // ---- Vote path ----------------------------------------------------------

    /// `Chits` — record a vote. The vote is bubbled to the nearest processing
    /// ancestor; when a poll completes, the result is recorded into consensus
    /// and the VM preference is updated.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn chits(
        &mut self,
        node: NodeId,
        req: u32,
        preferred_id: Id,
        preferred_id_at_height: Id,
        accepted_id: Id,
        accepted_height: u64,
    ) -> Result<()> {
        self.accepted_frontiers
            .insert(node, (accepted_id, accepted_height));

        // Try to issue the preferred block(s) so the vote has a target.
        self.issue_from_by_id(node, preferred_id).await?;
        if preferred_id != preferred_id_at_height {
            self.issue_from_by_id(node, preferred_id_at_height).await?;
        }

        // Bubble the vote: first option that maps to a processing ancestor wins.
        let mut applied = self.apply_vote(req, node, preferred_id);
        if applied.is_none() && preferred_id != preferred_id_at_height {
            applied = self.apply_vote(req, node, preferred_id_at_height);
        }
        let results = match applied {
            Some(results) => results,
            None => self.polls.drop(req, node),
        };

        self.process_poll_results(results).await
    }

    /// `QueryFailed` — synthesize a self-vote for the peer's last-accepted block,
    /// or drop the poll if we have no frontier for them.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn query_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        if let Some((accepted_id, accepted_height)) = self.accepted_frontiers.get(&node).copied() {
            return self
                .chits(
                    node,
                    req,
                    accepted_id,
                    accepted_id,
                    accepted_id,
                    accepted_height,
                )
                .await;
        }
        let results = self.polls.drop(req, node);
        self.process_poll_results(results).await
    }

    /// Bubble `vote` to the nearest processing ancestor and register it; returns
    /// the completed-poll results if the vote could be applied.
    fn apply_vote(&mut self, req: u32, node: NodeId, vote: Id) -> Option<Vec<Bag<Id>>> {
        let ancestor = self.get_processing_ancestor(vote)?;
        Some(self.polls.vote(req, node, ancestor))
    }

    /// `getProcessingAncestor` — find `initial_vote`'s nearest processing
    /// ancestor. If `initial_vote` is itself processing it is returned.
    fn get_processing_ancestor(&self, initial_vote: Id) -> Option<Id> {
        let mut current = initial_vote;
        loop {
            if self.consensus.processing(current) {
                return Some(current);
            }
            // Walk up via consensus' parent map (the unverified ancestor tree is
            // folded into consensus here).
            match self.consensus.get_parent(current) {
                Some(parent) => current = parent,
                None => return None,
            }
        }
    }

    /// Apply each completed poll's result to consensus and update preference.
    async fn process_poll_results(&mut self, results: Vec<Bag<Id>>) -> Result<()> {
        if results.is_empty() {
            return Ok(());
        }
        for result in results {
            self.consensus.record_poll(&result)?;
        }
        self.set_vm_preference().await?;

        if self.consensus.num_processing() > 0 {
            self.repoll().await?;
        }
        Ok(())
    }

    async fn set_vm_preference(&mut self) -> Result<()> {
        let pref = self.consensus.preference();
        let mut vm = self.cfg.vm.lock().await;
        vm.set_preference(&self.cfg.token, pref).await?;
        Ok(())
    }

    // ---- VM notify ----------------------------------------------------------

    /// `Notify(PendingTxs)` — schedule a build-block attempt and drain it.
    ///
    /// # Errors
    /// Propagates a fatal VM/consensus error.
    pub async fn notify_pending_txs(&mut self) -> Result<()> {
        self.pending_build_blocks = self.pending_build_blocks.saturating_add(1);
        self.build_blocks().await
    }

    /// `buildBlocks` — while there are pending build requests and processing is
    /// below `optimal_processing`, build + issue a block.
    async fn build_blocks(&mut self) -> Result<()> {
        while self.pending_build_blocks > 0
            && u32::try_from(self.consensus.num_processing()).unwrap_or(u32::MAX)
                < self.cfg.params.optimal_processing
        {
            self.pending_build_blocks = self.pending_build_blocks.saturating_sub(1);
            let blk = {
                let mut vm = self.cfg.vm.lock().await;
                vm.build_block(&self.cfg.token).await
            };
            let blk = match blk {
                Ok(blk) => blk,
                // VM declined to build a block.
                Err(_) => return Ok(()),
            };
            let node = NodeId::from([0u8; 20]);
            self.issue_from(node, blk).await?;
        }
        Ok(())
    }

    // ---- predicates ---------------------------------------------------------

    /// `shouldIssueBlock` — true if the block isn't decided, pending, or already
    /// processing.
    fn should_issue_block(&self, blk: &dyn ava_snow::Block) -> bool {
        if self.is_decided(blk) {
            return false;
        }
        !self.consensus.processing(blk.id())
    }

    /// `canIssueChildOn` — a child of `parent_id` may be verified/added iff the
    /// parent is the last-accepted block or is processing.
    fn can_issue_child_on(&self, parent_id: Id) -> bool {
        let (last_accepted_id, _) = self.consensus.last_accepted();
        parent_id == last_accepted_id || self.consensus.processing(parent_id)
    }

    /// `isDecided` — a block whose height is at or below the last-accepted height
    /// is decided (accepted or rejected).
    fn is_decided(&self, blk: &dyn ava_snow::Block) -> bool {
        let (_, last_accepted_height) = self.consensus.last_accepted();
        blk.height() <= last_accepted_height
    }
}
