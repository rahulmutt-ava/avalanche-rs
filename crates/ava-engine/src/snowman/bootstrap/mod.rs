// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman bootstrapper (port of `snow/engine/snowman/bootstrap/`, specs 06
//! §4.3).
//!
//! The bootstrapper brings a fresh (or restarted) node up to the network tip
//! before normal-operation consensus begins:
//!
//! 1. **Frontier discovery** — `SendGetAcceptedFrontier` to a beacon set; collect
//!    each beacon's last-accepted block id.
//! 2. **Frontier agreement** — `SendGetAccepted` with the union of frontiers;
//!    keep the ids that a *weight threshold* of beacons report accepted.
//! 3. **Fetch ancestry** — `SendGetAncestors` for each accepted tip; parse the
//!    `Ancestors` reply into the [`interval`] tree, requesting parents until the
//!    range connects back to the local last-accepted height.
//! 4. **Execute** — replay/verify/accept the fetched range in height order via
//!    [`acceptor::execute`], setting `ConsensusContext.executing`.
//! 5. **Handoff** — transition `EngineState::Bootstrapping → NormalOp` and invoke
//!    `on_finished`.
//!
//! The bootstrapper is `Halter`-aware via a [`CancellationToken`]: cancelling it
//! aborts the (potentially long) execute pass promptly.
//!
//! ## Port note
//!
//! Go's bootstrapper carries a `StartupTracker`, `PeerTracker` bandwidth
//! sampling, ETA tracking, DB batching, and genesis checkpoints. This port keeps
//! the essential state machine (the five steps above) and a weight-threshold
//! beacon model, driving fetch from a round-robin over the beacon set. The
//! interval tree + height-ordered execute are faithful (see [`interval`] /
//! [`acceptor`]). See `tests/PORTING.md`.

pub mod acceptor;
pub mod interval;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_snow::state::EngineState;
use ava_snow::{Block as VmBlock, ConsensusContext};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::block::{ChainVm, batched_parse_block};

use crate::common::sender::Sender;
use crate::error::{Error, Result};
use crate::snowman::bootstrap::interval::{Blocks, Tree, add_block};

/// The bootstrapper's lifecycle phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not yet started.
    Initializing,
    /// Awaiting `AcceptedFrontier` replies from the beacons.
    DiscoveringFrontier,
    /// Awaiting `Accepted` replies from the beacons.
    AgreeingFrontier,
    /// Fetching ancestry via `GetAncestors`.
    Fetching,
    /// Replaying the fetched range.
    Executing,
    /// Handed off to normal operation.
    Finished,
}

/// `bootstrap.Config` — the bootstrapper's dependencies.
pub struct Config<V, S> {
    /// The subnet this chain belongs to.
    pub subnet_id: Id,
    /// The consensus context (acceptor + `executing`/`state` phase flags).
    pub ctx: Arc<ConsensusContext>,
    /// The VM, behind a mutex (single-owner task).
    pub vm: Arc<Mutex<V>>,
    /// The outbound sender.
    pub sender: Arc<S>,
    /// The beacon set with stake weights (frontier-agreement threshold input).
    pub beacons: BTreeMap<NodeId, u64>,
    /// Halt signal.
    pub token: CancellationToken,
}

impl<V, S> Config<V, S> {
    /// The total weight of the beacon set.
    fn total_weight(&self) -> u64 {
        self.beacons.values().copied().fold(0, u64::saturating_add)
    }

    /// The weight threshold an id must reach to be accepted into the frontier:
    /// strictly more than half the beacon stake (Go uses an `Alpha`-style weight
    /// majority; we use a simple `> total/2` quorum for the in-memory model).
    fn weight_threshold(&self) -> u64 {
        self.total_weight() / 2
    }
}

/// The Snowman bootstrapper state machine. Generic over the VM and [`Sender`].
///
/// `on_finished` is invoked with the last request id when the node has caught up
/// and is ready for normal operation (Go `onFinished`).
pub struct Bootstrapper<V, S> {
    cfg: Config<V, S>,
    phase: Phase,
    request_id: u32,
    /// Last-accepted height read from the VM at start.
    last_accepted_height: u64,
    /// Beacons that have replied with an `AcceptedFrontier`.
    frontier_replies: BTreeMap<NodeId, Id>,
    /// Beacons that have responded to the frontier query (reply **or** failure).
    /// Phase completion is keyed on this set, not `frontier_replies`, so a
    /// failed/absent beacon (empty opinion) still advances discovery.
    frontier_responded: BTreeSet<NodeId>,
    /// Per-id accepted weight (frontier-agreement tally).
    accepted_weight: BTreeMap<Id, u64>,
    /// Beacons that have replied with an `Accepted`.
    accepted_replies: BTreeSet<NodeId>,
    /// The accepted tips whose ancestry must be fetched.
    accepted_tips: BTreeSet<Id>,
    /// Outstanding `GetAncestors` requests: `(node, request_id) -> wanted blkID`.
    outstanding: BTreeMap<(NodeId, u32), Id>,
    /// The interval tree of fetched heights.
    tree: Tree,
    /// The fetched block bytes by height.
    blocks: Blocks,
    /// Round-robin cursor over the beacon set for fetch peer selection.
    fetch_cursor: usize,
    /// Whether `on_finished` has fired.
    finished: bool,
}

impl<V, S> Bootstrapper<V, S>
where
    V: ChainVm,
    S: Sender,
{
    /// Builds a bootstrapper in the `Initializing` phase.
    #[must_use]
    pub fn new(cfg: Config<V, S>) -> Self {
        Self {
            cfg,
            phase: Phase::Initializing,
            request_id: 0,
            last_accepted_height: 0,
            frontier_replies: BTreeMap::new(),
            frontier_responded: BTreeSet::new(),
            accepted_weight: BTreeMap::new(),
            accepted_replies: BTreeSet::new(),
            accepted_tips: BTreeSet::new(),
            outstanding: BTreeMap::new(),
            tree: Tree::new(),
            blocks: Blocks::new(),
            fetch_cursor: 0,
            finished: false,
        }
    }

    /// The current lifecycle phase.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The last request id issued (exposed for tests / handoff).
    #[must_use]
    pub fn request_id(&self) -> u32 {
        self.request_id
    }

    fn beacon_ids(&self) -> Vec<NodeId> {
        self.cfg.beacons.keys().copied().collect()
    }

    fn check_halt(&self) -> Result<()> {
        if self.cfg.token.is_cancelled() {
            return Err(Error::Halted);
        }
        Ok(())
    }

    /// `Start` — set the engine phase to `Bootstrapping` and begin frontier
    /// discovery.
    ///
    /// # Errors
    /// [`Error::Halted`] if the token has fired; propagates a VM error reading
    /// the last-accepted height.
    pub async fn start(&mut self, start_req_id: u32) -> Result<()> {
        self.check_halt()?;
        self.request_id = start_req_id;
        self.cfg
            .ctx
            .state
            .store(Arc::new(EngineState::Bootstrapping));

        // Read the local last-accepted height.
        self.last_accepted_height = {
            let vm = self.cfg.vm.lock().await;
            let last_accepted_id = vm.last_accepted(&self.cfg.token).await?;
            let blk = vm.get_block(&self.cfg.token, last_accepted_id).await?;
            blk.height()
        };

        // Step 1: frontier discovery.
        self.phase = Phase::DiscoveringFrontier;
        self.request_id = self.request_id.wrapping_add(1);
        let beacons: std::collections::HashSet<NodeId> = self.cfg.beacons.keys().copied().collect();
        self.cfg
            .sender
            .send_get_accepted_frontier(&beacons, self.request_id);

        // If there are no beacons, nothing to fetch — go straight to handoff.
        if self.cfg.beacons.is_empty() {
            return self.finish().await;
        }
        Ok(())
    }

    /// `AcceptedFrontier` — record a beacon's last-accepted frontier id.
    ///
    /// # Errors
    /// Propagates a VM/acceptor error from the agreement/fetch that may follow.
    pub async fn accepted_frontier(
        &mut self,
        node: NodeId,
        _req: u32,
        container_id: Id,
    ) -> Result<()> {
        if self.phase != Phase::DiscoveringFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.frontier_responded.insert(node) {
            return Ok(()); // duplicate response
        }
        self.frontier_replies.insert(node, container_id);
        self.maybe_begin_frontier_agreement();
        Ok(())
    }

    /// `GetAcceptedFrontierFailed` — the beacon did not answer the frontier
    /// query (request timed out / never connected). Records an *empty opinion*
    /// (Go `minority.RecordOpinion(node, nil)`): it counts toward phase
    /// completion but contributes no frontier id, so a slow/absent beacon
    /// cannot stall discovery.
    ///
    /// # Errors
    /// Propagates a VM/acceptor error from the agreement that may follow.
    pub async fn get_accepted_frontier_failed(&mut self, node: NodeId, _req: u32) -> Result<()> {
        if self.phase != Phase::DiscoveringFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.frontier_responded.insert(node) {
            return Ok(()); // duplicate response
        }
        self.maybe_begin_frontier_agreement();
        Ok(())
    }

    /// Once every beacon has responded (reply or failure): begin agreement if
    /// any frontier was reported, otherwise restart discovery (all beacons
    /// failed — no frontier information at all; Go "no blocks accepted →
    /// restart bootstrap").
    fn maybe_begin_frontier_agreement(&mut self) {
        if self.frontier_responded.len() != self.cfg.beacons.len() {
            return;
        }
        if self.frontier_replies.is_empty() {
            self.restart_frontier_discovery();
            return;
        }
        self.begin_frontier_agreement();
    }

    /// Re-broadcast `GetAcceptedFrontier` under a fresh request id after every
    /// beacon failed, clearing the responded set so the new round can complete.
    fn restart_frontier_discovery(&mut self) {
        self.frontier_responded.clear();
        self.frontier_replies.clear();
        self.request_id = self.request_id.wrapping_add(1);
        let beacons: std::collections::HashSet<NodeId> = self.cfg.beacons.keys().copied().collect();
        self.cfg
            .sender
            .send_get_accepted_frontier(&beacons, self.request_id);
    }

    fn begin_frontier_agreement(&mut self) {
        self.phase = Phase::AgreeingFrontier;
        let frontier: BTreeSet<Id> = self.frontier_replies.values().copied().collect();
        let ids: Vec<Id> = frontier.into_iter().collect();
        self.request_id = self.request_id.wrapping_add(1);
        let beacons: std::collections::HashSet<NodeId> = self.cfg.beacons.keys().copied().collect();
        self.cfg
            .sender
            .send_get_accepted(&beacons, self.request_id, &ids);
    }

    /// `Accepted` — record a beacon's accepted subset; once all beacons reply,
    /// tally the weight-threshold frontier and begin fetching.
    ///
    /// # Errors
    /// Propagates a VM error from the fetch that follows.
    pub async fn accepted(&mut self, node: NodeId, _req: u32, container_ids: &[Id]) -> Result<()> {
        if self.phase != Phase::AgreeingFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.accepted_replies.insert(node) {
            return Ok(()); // duplicate reply
        }
        let weight = self.cfg.beacons.get(&node).copied().unwrap_or(0);
        for &id in container_ids {
            let entry = self.accepted_weight.entry(id).or_insert(0);
            *entry = entry.saturating_add(weight);
        }

        if self.accepted_replies.len() == self.cfg.beacons.len() {
            self.begin_fetching().await?;
        }
        Ok(())
    }

    /// `GetAcceptedFailed` — the beacon did not answer the frontier-agreement
    /// query. Records an *empty opinion* (Go `majority.RecordOpinion(node, nil)`):
    /// it counts toward phase completion but contributes no accepted weight.
    ///
    /// # Errors
    /// Propagates a VM error from the fetch that may follow.
    pub async fn get_accepted_failed(&mut self, node: NodeId, _req: u32) -> Result<()> {
        if self.phase != Phase::AgreeingFrontier || !self.cfg.beacons.contains_key(&node) {
            return Ok(());
        }
        if !self.accepted_replies.insert(node) {
            return Ok(()); // duplicate response
        }
        if self.accepted_replies.len() == self.cfg.beacons.len() {
            self.begin_fetching().await?;
        }
        Ok(())
    }

    async fn begin_fetching(&mut self) -> Result<()> {
        let threshold = self.cfg.weight_threshold();
        self.accepted_tips = self
            .accepted_weight
            .iter()
            .filter(|&(_, &w)| w > threshold)
            .map(|(&id, _)| id)
            .collect();

        self.phase = Phase::Fetching;

        if self.accepted_tips.is_empty() {
            // Nothing the network agrees on above our tip: we are caught up.
            return self.try_start_executing().await;
        }

        let tips: Vec<Id> = self.accepted_tips.iter().copied().collect();
        for tip in tips {
            self.fetch(tip);
        }
        self.try_start_executing().await
    }

    /// `fetch` — request `blk_id` and its ancestors from the next beacon.
    fn fetch(&mut self, blk_id: Id) {
        // Skip if already outstanding for this block.
        if self.outstanding.values().any(|&v| v == blk_id) {
            return;
        }
        let beacons = self.beacon_ids();
        if beacons.is_empty() {
            return;
        }
        // `beacons` is non-empty (checked above), so the divisor is non-zero.
        #[allow(clippy::arithmetic_side_effects)]
        let node = beacons[self.fetch_cursor % beacons.len()];
        self.fetch_cursor = self.fetch_cursor.wrapping_add(1);

        self.request_id = self.request_id.wrapping_add(1);
        self.outstanding.insert((node, self.request_id), blk_id);
        self.cfg
            .sender
            .send_get_ancestors(node, self.request_id, blk_id);
    }

    /// `Ancestors` — handle a chain of fetched block bytes (oldest-last). Adds
    /// them to the interval tree, requesting any still-missing parent.
    ///
    /// # Errors
    /// Propagates a VM parse / acceptor / verify / accept error.
    pub async fn ancestors(
        &mut self,
        node: NodeId,
        req: u32,
        containers: &[Vec<u8>],
    ) -> Result<()> {
        let Some(wanted) = self.outstanding.remove(&(node, req)) else {
            return Ok(());
        };
        if containers.is_empty() {
            // Empty reply: retry the wanted block from another peer.
            self.fetch(wanted);
            return Ok(());
        }

        let parsed = {
            let vm = self.cfg.vm.lock().await;
            batched_parse_block(&*vm, &self.cfg.token, containers).await?
        };
        if parsed.is_empty() {
            self.fetch(wanted);
            return Ok(());
        }
        // The first block must be the one we requested.
        if parsed[0].id() != wanted {
            self.fetch(wanted);
            return Ok(());
        }

        self.process_chain(&parsed);
        self.try_start_executing().await
    }

    /// `GetAncestorsFailed` — retry the wanted block from another peer.
    ///
    /// # Errors
    /// Propagates a VM error from the retry/execute path.
    pub async fn get_ancestors_failed(&mut self, node: NodeId, req: u32) -> Result<()> {
        if let Some(wanted) = self.outstanding.remove(&(node, req)) {
            self.fetch(wanted);
        }
        Ok(())
    }

    /// `process` — add a chain of blocks to the tree, requesting the first
    /// still-missing parent (Go `bootstrapper.process` + `storage.process`).
    fn process_chain(&mut self, chain: &[Arc<dyn VmBlock>]) {
        // Build an ancestors lookup so we can walk parents within the reply.
        let by_id: BTreeMap<Id, &Arc<dyn VmBlock>> = chain.iter().map(|b| (b.id(), b)).collect();

        let mut current = &chain[0];
        loop {
            let height = current.height();
            let wants_parent = add_block(
                &mut self.tree,
                &mut self.blocks,
                self.last_accepted_height,
                height,
                current.bytes().to_vec(),
            );
            if !wants_parent {
                return;
            }
            let parent_id = current.parent();
            match by_id.get(&parent_id) {
                Some(parent) => current = parent,
                None => {
                    // Parent not in this reply: request it.
                    self.fetch(parent_id);
                    return;
                }
            }
        }
    }

    /// `tryStartExecuting` — once no fetches are outstanding, execute the range
    /// and hand off.
    async fn try_start_executing(&mut self) -> Result<()> {
        if !self.outstanding.is_empty() {
            return Ok(()); // still fetching
        }
        if self.phase == Phase::Finished {
            return Ok(());
        }

        self.phase = Phase::Executing;
        acceptor::execute(
            &self.cfg.token,
            &self.cfg.ctx,
            &self.cfg.vm,
            &mut self.tree,
            &mut self.blocks,
            self.last_accepted_height,
        )
        .await?;

        self.finish().await
    }

    /// Transition to normal operation.
    async fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.phase = Phase::Finished;
        self.cfg.ctx.state.store(Arc::new(EngineState::NormalOp));
        Ok(())
    }

    /// Whether the bootstrapper has handed off to normal operation.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}
