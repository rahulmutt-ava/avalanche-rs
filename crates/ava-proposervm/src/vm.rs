// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ProposerVm` wrapper (Go `vms/proposervm/vm.go` + `pre_fork_block.go` +
//! `post_fork_block.go` + `block.go::buildChild`).
//!
//! `ProposerVm<V, S>` is a VM **middleware**: it wraps an inner [`ChainVm`] and
//! presents itself as a [`ChainVm`] to the engine, adding the Snowman++ proposer
//! windows that throttle block production. It owns:
//!
//! - a [`Windower<S>`] to decide which validator may propose at a given
//!   height/slot (M3.22, R1-confirmed);
//! - the persisted [`State`] (chain state + the `height -> blockID` index, M3.23);
//! - the fork schedule ([`UpgradeConfig`]) selecting the block regime by the
//!   inner block timestamp/height (pre-fork / post-fork pre-Durango /
//!   post-Durango / Granite);
//! - an [`Arc<dyn Clock>`] for the slot-wait build path (virtual-time friendly);
//! - the proposer's [`StakingIdentity`] (cert + signer) used to sign post-fork
//!   blocks.
//!
//! ## Scope notes (see `tests/PORTING.md`)
//!
//! This port covers the fork-regime selection, the height index + inner-VM
//! delegation, and the slot-wait sign/build path. The full Go VM additionally
//! implements oracle/option wrapping, the verified-block graph + inner-block
//! cache, height-repair/pruning (`NumHistoricalBlocks`), epoch (ACP-181)
//! selection, and the HTTP/RPC service — those are explicit deferrals recorded
//! in `tests/PORTING.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use tokio_util::sync::CancellationToken;

use ava_database::DynDatabase;
use ava_snow::{Block as SnowBlock, ChainContext, EngineState};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::Clock;
use ava_validators::state::ValidatorState;
use ava_version::application::Application;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::block::{BatchedChainVm, Block, ChainVm, StateSyncableVm};
use ava_vm::connector::Connector;
use ava_vm::error::Error as VmError;
use ava_vm::error::Result as VmResult;
use ava_vm::health::HealthCheck;
use ava_vm::vm::{Fx, HttpHandler, Vm, VmEvent};

use crate::block::{GraniteBlock, SignedBlock};
use crate::error::Error;
use crate::height_index::{self, HeightLookup};
use crate::proposer::windower::{MAX_BUILD_WINDOWS, Windower, time_to_slot};
use crate::state::State;

/// `maxSkew` — the maximum time a block may be ahead of local time (24 §B.3).
pub const MAX_SKEW: Duration = Duration::from_secs(10);

/// A signing closure over the header bytes (the `ring`/Go `crypto.Signer`
/// hashes the message internally with SHA-256).
pub type BlockSigner = Arc<dyn Fn(&[u8]) -> std::result::Result<Vec<u8>, String> + Send + Sync>;

/// The proposer's staking identity: the DER-encoded cert plus a signer.
#[derive(Clone)]
pub struct StakingIdentity {
    /// The DER-encoded staking certificate (`statelessUnsignedBlock.certificate`).
    pub certificate: Vec<u8>,
    /// Signs the proposervm `Header` bytes.
    pub signer: BlockSigner,
}

impl std::fmt::Debug for StakingIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StakingIdentity")
            .field("certificate_len", &self.certificate.len())
            .finish_non_exhaustive()
    }
}

/// The proposervm-shared state a wrapper block reaches into on `accept`.
struct Shared {
    state: State,
    /// The last-accepted proposervm height (genesis is pre-fork ⇒ 0).
    last_accepted_height: Mutex<u64>,
    /// In-memory cache of post-fork blocks that have been built/parsed but not
    /// yet persisted (Go `verifiedBlocks`). Keyed by proposervm block id →
    /// serialized bytes. Lets `set_preference`/`get_block` resolve a freshly
    /// built block before it is accepted.
    verified: Mutex<HashMap<Id, Vec<u8>>>,
}

impl Shared {
    /// Records a post-fork block's bytes in the in-memory cache.
    fn remember(&self, id: Id, bytes: Vec<u8>) {
        self.verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, bytes);
    }

    /// Resolves a post-fork block's bytes from the in-memory cache or the state
    /// DB (Go `getStatelessBlk`).
    fn block_bytes(&self, id: Id) -> Option<Vec<u8>> {
        if let Some(bytes) = self
            .verified
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            return Some(bytes.clone());
        }
        self.state.get_block(id).ok()
    }
}

/// `ProposerVm` — the Snowman++ proposer-window middleware over an inner VM.
pub struct ProposerVm<V: ChainVm, S: ValidatorState> {
    inner: V,
    ctx: Arc<ChainContext>,
    clock: Arc<dyn Clock>,
    windower: Windower<S>,
    shared: Arc<Shared>,
    identity: Option<StakingIdentity>,
    consensus_state: EngineState,
    /// The currently preferred proposervm block id (`Id::EMPTY` until set).
    preferred: Id,
}

impl<V: ChainVm, S: ValidatorState> ProposerVm<V, S> {
    /// Builds a `ProposerVm` over `inner`. The validator state `S` resolves the
    /// windower's validator sets; `db` backs the proposervm state/index.
    pub fn new(
        inner: V,
        ctx: Arc<ChainContext>,
        clock: Arc<dyn Clock>,
        validator_state: S,
        db: Arc<dyn DynDatabase>,
        identity: Option<StakingIdentity>,
    ) -> Self {
        let windower = Windower::new(validator_state, ctx.subnet_id, ctx.chain_id);
        let shared = Arc::new(Shared {
            state: State::new(db),
            last_accepted_height: Mutex::new(0),
            verified: Mutex::new(HashMap::new()),
        });
        Self {
            inner,
            ctx,
            clock,
            windower,
            shared,
            identity,
            consensus_state: EngineState::Initializing,
            preferred: Id::EMPTY,
        }
    }

    /// Wires up the wrapper after the inner VM is initialized: seeds the
    /// preferred id from the inner VM's last accepted block (Go
    /// `Initialize`/`setLastAcceptedMetadata`). The inner VM MUST already be
    /// `initialize`d (the engine initializes the inner VM via `Vm::initialize`).
    ///
    /// # Errors
    /// Propagates the inner VM's `last_accepted` error.
    pub async fn initialize_wrapper(&mut self, token: &CancellationToken) -> VmResult<()> {
        // The proposervm's preferred starts at the (proposervm) last-accepted,
        // which is the inner last-accepted pre-fork.
        let last = self.last_accepted(token).await?;
        self.preferred = last;
        Ok(())
    }

    /// Whether the ProposerVM fork is active at `parent_timestamp` (Go
    /// `IsApricotPhase4Activated`).
    fn fork_active(&self, parent_timestamp: SystemTime) -> bool {
        self.ctx
            .network_upgrades
            .is_apricot_phase_4_activated(to_datetime(parent_timestamp))
    }

    fn durango_active(&self, t: SystemTime) -> bool {
        self.ctx
            .network_upgrades
            .is_durango_activated(to_datetime(t))
    }

    /// Resolves a proposervm block id to its parsed post-fork form, or `None`
    /// if it is a pre-fork (inner) block.
    fn get_post_fork(&self, blk_id: Id) -> Option<crate::block::ParsedBlock> {
        let bytes = self.shared.block_bytes(blk_id)?;
        crate::block::parse_without_verification(&bytes).ok()
    }

    /// The build path (Go `BuildBlock` → `buildChild`). Selects the regime by the
    /// preferred block's timestamp, waits for this node's slot, and signs.
    async fn build_child(&mut self, token: &CancellationToken) -> VmResult<Arc<dyn Block>> {
        let preferred = self.preferred;
        let post_fork = self.get_post_fork(preferred);

        // Determine the parent's (timestamp, p_chain_height, inner height).
        let (parent_timestamp, parent_p_chain_height, is_post_fork_parent) = match &post_fork {
            Some(crate::block::ParsedBlock::Signed(sb)) => {
                (from_unix_secs(sb.timestamp()), sb.p_chain_height(), true)
            }
            Some(crate::block::ParsedBlock::Granite(gb)) => {
                (from_unix_secs(gb.timestamp()), gb.p_chain_height(), true)
            }
            // Option blocks / unknown ⇒ treat as pre-fork parent for timestamp.
            _ => {
                let inner = self.inner.get_block(token, preferred).await?;
                (inner.timestamp(), 0u64, false)
            }
        };

        // Pre-fork regime: the chain hasn't forked yet → bare inner block.
        if !self.fork_active(parent_timestamp) {
            let inner = self.inner.build_block(token).await?;
            return Ok(inner);
        }

        // Post-fork. Compute the child timestamp (later of now and parent).
        let now = self.clock.unix_time();
        let new_timestamp = if now < parent_timestamp {
            parent_timestamp
        } else {
            now
        };

        // The inner block we will wrap.
        let inner = self.inner.build_block(token).await?;
        let inner_height = inner.height();
        let inner_bytes = inner.bytes().to_vec();
        let parent_id = preferred;

        // Pre-fork parent → the first post-fork (transition) block is ALWAYS
        // unsigned with no proposer (Go `preForkBlock.buildChild`).
        if !is_post_fork_parent {
            let p_chain_height = self.select_child_p_chain_height(0).await;
            let stateless = SignedBlock::build_unsigned(
                parent_id,
                to_unix_secs(new_timestamp),
                p_chain_height,
                inner_bytes,
            )?;
            return Ok(self.wrap_signed(stateless, inner, inner_height));
        }

        // Child of a post-fork block: wait for this node's slot, then sign.
        let p_chain_height = self
            .select_child_p_chain_height(parent_p_chain_height)
            .await;
        let should_sign = self
            .wait_for_slot_and_decide(
                parent_id,
                parent_timestamp,
                parent_p_chain_height,
                inner_height,
                &mut Some(new_timestamp),
            )
            .await?;

        // Recompute the child timestamp after any wait (clock may have advanced).
        let now = self.clock.unix_time();
        let new_timestamp = if now < parent_timestamp {
            parent_timestamp
        } else {
            now
        };

        let stateless = if should_sign {
            let identity = self.identity.as_ref().ok_or(VmError::InvalidComponent(
                "proposervm: node is the proposer but has no staking identity",
            ))?;
            let signer = Arc::clone(&identity.signer);
            SignedBlock::build_signed(
                parent_id,
                to_unix_secs(new_timestamp),
                p_chain_height,
                identity.certificate.clone(),
                inner_bytes,
                self.ctx.chain_id,
                &move |msg: &[u8]| (signer)(msg),
            )?
        } else {
            SignedBlock::build_unsigned(
                parent_id,
                to_unix_secs(new_timestamp),
                p_chain_height,
                inner_bytes,
            )?
        };
        Ok(self.wrap_signed(stateless, inner, inner_height))
    }

    /// `selectChildPChainHeight` (simplified): the optimal P-Chain height is at
    /// least the parent's. We query the validator state's minimum height and
    /// take the max (Go `selectChildPChainHeight` without the Fuji override).
    async fn select_child_p_chain_height(&self, min_p_chain_height: u64) -> u64 {
        match self.windower.validator_state().get_minimum_height().await {
            Ok(recommended) => recommended.max(min_p_chain_height),
            Err(_) => min_p_chain_height,
        }
    }

    /// Waits until it is this node's slot (post-Durango) or window (pre-Durango)
    /// and returns whether to build a *signed* block. Mirrors Go
    /// `shouldBuildSignedBlock{PostDurango,PreDurango}` + the `WaitForEvent`
    /// slot wait, collapsed into the build path (the task's design).
    async fn wait_for_slot_and_decide(
        &self,
        parent_id: Id,
        parent_timestamp: SystemTime,
        parent_p_chain_height: u64,
        parent_inner_height: u64,
        new_timestamp: &mut Option<SystemTime>,
    ) -> VmResult<bool> {
        let child_height = parent_inner_height.saturating_add(1);

        if self.durango_active(parent_timestamp) {
            // Post-Durango: discrete per-slot proposers.
            let current = self.clock.unix_time();
            let slot = time_to_slot(
                dur_since_epoch(parent_timestamp),
                dur_since_epoch(current.max(parent_timestamp)),
            );
            // Compute the minimum delay until this node's slot.
            let slot_time = match self
                .windower
                .min_delay_for_proposer(child_height, parent_p_chain_height, self.ctx.node_id, slot)
                .await
            {
                Ok(delay) => parent_timestamp.checked_add(delay),
                // No validators ⇒ anyone can propose (unsigned, no wait).
                Err(Error::AnyoneCanPropose) => {
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            };

            // Wait until the slot time (virtual clock).
            if let Some(slot_time) = slot_time {
                self.wait_until(slot_time).await;
            }
            let _ = new_timestamp;

            // Decide: is this node the expected proposer for the current slot?
            let now = self.clock.unix_time();
            let slot_now = time_to_slot(
                dur_since_epoch(parent_timestamp),
                dur_since_epoch(now.max(parent_timestamp)),
            );
            match self
                .windower
                .expected_proposer(child_height, parent_p_chain_height, slot_now)
                .await
            {
                Ok(expected) if expected == self.ctx.node_id => Ok(true),
                Ok(_) => {
                    let _ = parent_id;
                    Err(Error::NotProposer.into())
                }
                Err(Error::AnyoneCanPropose) => Ok(false),
                Err(e) => Err(e.into()),
            }
        } else {
            // Pre-Durango: proposer windows from the parent timestamp onward.
            let delay = self
                .windower
                .delay(
                    child_height,
                    parent_p_chain_height,
                    self.ctx.node_id,
                    MAX_BUILD_WINDOWS,
                )
                .await
                .map_err(VmError::from)?;
            let slot_time = parent_timestamp.checked_add(delay);
            if let Some(slot_time) = slot_time {
                self.wait_until(slot_time).await;
            }
            // A signed block is built within the verify window; here we sign
            // whenever it is our window (the unsigned/max-delay path is reached
            // only when no validator wins — not modeled in this port).
            Ok(true)
        }
    }

    /// Waits (virtual clock) until `target`, polling the injected [`Clock`].
    async fn wait_until(&self, target: SystemTime) {
        loop {
            let now = self.clock.unix_time();
            let Ok(remaining) = target.duration_since(now) else {
                return; // target reached or in the past
            };
            if remaining.is_zero() {
                return;
            }
            // Sleep on the runtime timer; under `start_paused` this advances the
            // virtual clock, and the MockClock is advanced in lockstep by the
            // test harness where needed. Cap the sleep so a non-advancing mock
            // clock cannot hang the build (defensive — single-validator tests
            // resolve to a zero delay).
            tokio::time::sleep(remaining.min(Duration::from_millis(1))).await;
            // If the mock clock did not advance, break to avoid a busy spin.
            if self.clock.unix_time() <= now {
                return;
            }
        }
    }

    /// Wraps a freshly-built signed/unsigned post-fork block into a
    /// [`ProposerBlock`].
    fn wrap_signed(
        &self,
        stateless: SignedBlock,
        inner: Arc<dyn Block>,
        inner_height: u64,
    ) -> Arc<dyn Block> {
        self.shared
            .remember(stateless.id(), stateless.bytes().to_vec());
        Arc::new(ProposerBlock {
            kind: BlockKind::Signed(stateless),
            inner,
            height: inner_height,
            shared: Arc::clone(&self.shared),
        })
    }
}

fn to_datetime(t: SystemTime) -> chrono::DateTime<Utc> {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Utc.timestamp_opt(i64::try_from(secs).unwrap_or(i64::MAX), 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap_or_default())
}

fn dur_since_epoch(t: SystemTime) -> Duration {
    t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO)
}

fn from_unix_secs(secs: i64) -> SystemTime {
    let secs = u64::try_from(secs).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(UNIX_EPOCH)
}

fn to_unix_secs(t: SystemTime) -> i64 {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    i64::try_from(secs).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// The wrapper block
// ---------------------------------------------------------------------------

enum BlockKind {
    /// A pre-fork block (the inner block, surfaced verbatim).
    PreFork,
    /// A post-fork signed/unsigned block.
    Signed(SignedBlock),
    /// A post-Granite block.
    Granite(GraniteBlock),
}

/// A ProposerVM block presented to the engine. On `accept` it persists itself +
/// updates the height index (post-fork), then accepts the inner block.
struct ProposerBlock {
    kind: BlockKind,
    inner: Arc<dyn Block>,
    height: u64,
    shared: Arc<Shared>,
}

#[async_trait]
impl Block for ProposerBlock {
    fn id(&self) -> Id {
        match &self.kind {
            BlockKind::PreFork => self.inner.id(),
            BlockKind::Signed(b) => b.id(),
            BlockKind::Granite(b) => b.id(),
        }
    }

    fn parent(&self) -> Id {
        match &self.kind {
            BlockKind::PreFork => self.inner.parent(),
            BlockKind::Signed(b) => b.parent_id(),
            BlockKind::Granite(b) => b.parent_id(),
        }
    }

    fn height(&self) -> u64 {
        self.height
    }

    fn timestamp(&self) -> SystemTime {
        match &self.kind {
            BlockKind::PreFork => self.inner.timestamp(),
            BlockKind::Signed(b) => from_unix_secs(b.timestamp()),
            BlockKind::Granite(b) => from_unix_secs(b.timestamp()),
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.kind {
            BlockKind::PreFork => self.inner.bytes(),
            BlockKind::Signed(b) => b.bytes(),
            BlockKind::Granite(b) => b.bytes(),
        }
    }

    async fn verify(&self, token: &CancellationToken) -> ava_snow::Result<()> {
        // Inner-block verification (the full post-fork verify graph — parent
        // timestamp monotonicity, proposer checks, P-Chain height bounds — is a
        // deferral; see tests/PORTING.md).
        self.inner.verify(token).await
    }

    async fn accept(&self, token: &CancellationToken) -> ava_snow::Result<()> {
        // Post-fork: persist the block, advance last-accepted + the height
        // index (Go `acceptPostForkBlock`), BEFORE accepting the inner block.
        if !matches!(self.kind, BlockKind::PreFork) {
            let id = self.id();
            self.shared
                .state
                .set_last_accepted(id)
                .map_err(ava_snow::Error::from)?;
            self.shared
                .state
                .put_block(id, self.bytes())
                .map_err(ava_snow::Error::from)?;
            height_index::update_height_index(&self.shared.state, self.height, id)
                .map_err(ava_snow::Error::from)?;
            *self
                .shared
                .last_accepted_height
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = self.height;
        }
        self.inner.accept(token).await
    }

    async fn reject(&self, token: &CancellationToken) -> ava_snow::Result<()> {
        self.inner.reject(token).await
    }
}

// ---------------------------------------------------------------------------
// Vm + ChainVm: delegate to the inner VM, overriding the proposer-aware ops.
// ---------------------------------------------------------------------------

#[async_trait]
impl<V: ChainVm, S: ValidatorState> AppHandler for ProposerVm<V, S> {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> VmResult<()> {
        self.inner
            .app_request(token, node, request_id, deadline, request)
            .await
    }
    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        self.inner
            .app_request_failed(token, node, request_id, err)
            .await
    }
    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        self.inner
            .app_response(token, node, request_id, response)
            .await
    }
    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        self.inner.app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl<V: ChainVm, S: ValidatorState> HealthCheck for ProposerVm<V, S> {
    async fn health_check(&self, token: &CancellationToken) -> VmResult<serde_json::Value> {
        self.inner.health_check(token).await
    }
}

#[async_trait]
impl<V: ChainVm, S: ValidatorState> Connector for ProposerVm<V, S> {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> VmResult<()> {
        self.inner.connected(token, node, version).await
    }
    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> VmResult<()> {
        self.inner.disconnected(token, node).await
    }
}

#[async_trait]
impl<V: ChainVm, S: ValidatorState> Vm for ProposerVm<V, S> {
    async fn initialize(
        &mut self,
        token: &CancellationToken,
        chain_ctx: Arc<ChainContext>,
        db: Arc<dyn DynDatabase>,
        genesis_bytes: &[u8],
        upgrade_bytes: &[u8],
        config_bytes: &[u8],
        fxs: Vec<Fx>,
        app_sender: Arc<dyn AppSender>,
    ) -> VmResult<()> {
        self.inner
            .initialize(
                token,
                chain_ctx,
                db,
                genesis_bytes,
                upgrade_bytes,
                config_bytes,
                fxs,
                app_sender,
            )
            .await?;
        self.initialize_wrapper(token).await
    }

    async fn set_state(&mut self, token: &CancellationToken, state: EngineState) -> VmResult<()> {
        self.inner.set_state(token, state).await?;
        self.consensus_state = state;
        Ok(())
    }

    async fn shutdown(&mut self, token: &CancellationToken) -> VmResult<()> {
        self.inner.shutdown(token).await
    }

    async fn version(&self, token: &CancellationToken) -> VmResult<String> {
        self.inner.version(token).await
    }

    async fn create_handlers(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<HashMap<String, HttpHandler>> {
        self.inner.create_handlers(token).await
    }

    async fn new_http_handler(
        &mut self,
        token: &CancellationToken,
    ) -> VmResult<Option<HttpHandler>> {
        self.inner.new_http_handler(token).await
    }

    async fn wait_for_event(&self, token: &CancellationToken) -> VmResult<VmEvent> {
        self.inner.wait_for_event(token).await
    }
}

#[async_trait]
impl<V: ChainVm, S: ValidatorState> ChainVm for ProposerVm<V, S> {
    async fn build_block(&mut self, token: &CancellationToken) -> VmResult<Arc<dyn Block>> {
        self.build_child(token).await
    }

    async fn get_block(&self, token: &CancellationToken, id: Id) -> VmResult<Arc<dyn Block>> {
        // Post-fork block (in-memory verified cache or persisted state)?
        if let Some(parsed) = self
            .shared
            .block_bytes(id)
            .and_then(|bytes| crate::block::parse_without_verification(&bytes).ok())
        {
            let inner_bytes = parsed.inner_block().to_vec();
            let inner = self.inner.parse_block(token, &inner_bytes).await?;
            let height = inner.height();
            let kind = match parsed {
                crate::block::ParsedBlock::Signed(b) => BlockKind::Signed(b),
                crate::block::ParsedBlock::Granite(b) => BlockKind::Granite(b),
                crate::block::ParsedBlock::Option(_) => {
                    return Err(VmError::InvalidComponent(
                        "proposervm: option blocks not supported",
                    ));
                }
            };
            return Ok(Arc::new(ProposerBlock {
                kind,
                inner,
                height,
                shared: Arc::clone(&self.shared),
            }));
        }
        // Pre-fork: delegate to the inner VM, wrapping the result.
        let inner = self.inner.get_block(token, id).await?;
        let height = inner.height();
        Ok(Arc::new(ProposerBlock {
            kind: BlockKind::PreFork,
            inner,
            height,
            shared: Arc::clone(&self.shared),
        }))
    }

    async fn parse_block(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn Block>> {
        // Try to parse as a post-fork proposervm block first.
        if let Ok(parsed) = crate::block::parse_without_verification(bytes) {
            let inner_bytes = parsed.inner_block().to_vec();
            if let Ok(inner) = self.inner.parse_block(token, &inner_bytes).await {
                let height = inner.height();
                self.shared.remember(parsed.id(), bytes.to_vec());
                let kind = match parsed {
                    crate::block::ParsedBlock::Signed(b) => BlockKind::Signed(b),
                    crate::block::ParsedBlock::Granite(b) => BlockKind::Granite(b),
                    crate::block::ParsedBlock::Option(_) => {
                        return Err(VmError::InvalidComponent(
                            "proposervm: option blocks not supported",
                        ));
                    }
                };
                return Ok(Arc::new(ProposerBlock {
                    kind,
                    inner,
                    height,
                    shared: Arc::clone(&self.shared),
                }));
            }
        }
        // Pre-fork: the bytes are the bare inner block.
        let inner = self.inner.parse_block(token, bytes).await?;
        let height = inner.height();
        Ok(Arc::new(ProposerBlock {
            kind: BlockKind::PreFork,
            inner,
            height,
            shared: Arc::clone(&self.shared),
        }))
    }

    async fn set_preference(&mut self, token: &CancellationToken, id: Id) -> VmResult<()> {
        self.preferred = id;
        // If the preferred block is post-fork, delegate the inner preference to
        // the inner block id; otherwise pass through (Go `SetPreference`).
        if let Some(parsed) = self.get_post_fork(id) {
            let inner_bytes = parsed.inner_block().to_vec();
            let inner = self.inner.parse_block(token, &inner_bytes).await?;
            return self.inner.set_preference(token, inner.id()).await;
        }
        self.inner.set_preference(token, id).await
    }

    async fn last_accepted(&self, token: &CancellationToken) -> VmResult<Id> {
        match self.shared.state.get_last_accepted() {
            Ok(id) => Ok(id),
            Err(Error::NotFound) => self.inner.last_accepted(token).await,
            Err(e) => Err(e.into()),
        }
    }

    async fn get_block_id_at_height(&self, token: &CancellationToken, height: u64) -> VmResult<Id> {
        match height_index::get_block_id_at_height(&self.shared.state, height) {
            Ok(HeightLookup::Proposer(id)) => Ok(id),
            Ok(HeightLookup::InnerVm) => self.inner.get_block_id_at_height(token, height).await,
            Err(Error::NotFound) => Err(VmError::NotFound),
            Err(e) => Err(e.into()),
        }
    }

    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        // Delegate iff the inner VM is batched.
        if self.inner.as_batched().is_some() {
            Some(self)
        } else {
            None
        }
    }

    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        if self.inner.as_state_syncable().is_some() {
            Some(self)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Batched + state-syncable delegation (Go batched_vm.go / state_syncable_vm.go).
// ---------------------------------------------------------------------------

#[async_trait]
impl<V: ChainVm, S: ValidatorState> BatchedChainVm for ProposerVm<V, S> {
    async fn get_ancestors(
        &self,
        token: &CancellationToken,
        blk_id: Id,
        max_blocks_num: usize,
        max_blocks_size: usize,
        max_retrieval_time: Duration,
    ) -> VmResult<Vec<Vec<u8>>> {
        match self.inner.as_batched() {
            Some(b) => {
                b.get_ancestors(
                    token,
                    blk_id,
                    max_blocks_num,
                    max_blocks_size,
                    max_retrieval_time,
                )
                .await
            }
            None => Err(VmError::RemoteVmNotImplemented),
        }
    }

    async fn batched_parse_block(
        &self,
        token: &CancellationToken,
        blks: &[Vec<u8>],
    ) -> VmResult<Vec<Arc<dyn Block>>> {
        match self.inner.as_batched() {
            Some(b) => b.batched_parse_block(token, blks).await,
            None => Err(VmError::RemoteVmNotImplemented),
        }
    }
}

#[async_trait]
impl<V: ChainVm, S: ValidatorState> StateSyncableVm for ProposerVm<V, S> {
    async fn state_sync_enabled(&self, token: &CancellationToken) -> VmResult<bool> {
        match self.inner.as_state_syncable() {
            Some(ss) => ss.state_sync_enabled(token).await,
            None => Ok(false),
        }
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn ava_vm::block::StateSummary>> {
        match self.inner.as_state_syncable() {
            Some(ss) => ss.get_ongoing_sync_state_summary(token).await,
            None => Err(VmError::StateSyncableVmNotImplemented),
        }
    }

    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> VmResult<Arc<dyn ava_vm::block::StateSummary>> {
        match self.inner.as_state_syncable() {
            Some(ss) => ss.get_last_state_summary(token).await,
            None => Err(VmError::StateSyncableVmNotImplemented),
        }
    }

    async fn parse_state_summary(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<Arc<dyn ava_vm::block::StateSummary>> {
        match self.inner.as_state_syncable() {
            Some(ss) => ss.parse_state_summary(token, bytes).await,
            None => Err(VmError::StateSyncableVmNotImplemented),
        }
    }

    async fn get_state_summary(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> VmResult<Arc<dyn ava_vm::block::StateSummary>> {
        match self.inner.as_state_syncable() {
            Some(ss) => ss.get_state_summary(token, height).await,
            None => Err(VmError::StateSyncableVmNotImplemented),
        }
    }
}

// `SnowBlock` is the trait the wrapper block implements (re-exported from
// `ava-snow` via `ava-vm`); keep the import edge explicit.
const _: fn() = || {
    fn _assert_impl<T: SnowBlock>() {}
    _assert_impl::<ProposerBlock>();
};
