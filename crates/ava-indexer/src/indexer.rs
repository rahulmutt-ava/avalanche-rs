// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Chain registration + indexer lifecycle (Go `indexer/indexer.go`; specs 12
//! §5, 17 §2.2 #20 / §3).
//!
//! [`ContainerIndexer`] mirrors Go's `indexer` struct:
//!
//! - **`register_chain`** skips non-Primary-Network subnets and
//!   already-indexed chains, enforces the incomplete-index safety rule, and
//!   creates per-chain indices under the byte prefixes `tx=0x01`, `vtx=0x02`,
//!   `block=0x03` (each a `chainID ‖ kind` prefixdb over the indexer's DB).
//!   Any failure on this path is **fatal**: the indexer closes itself, which
//!   fires the shutdown callback (Go `log.Fatal` + `close()`).
//! - Each created index spawns one **acceptor task** (17 §2.2 #20) draining
//!   that chain's [`AcceptorGroup`] broadcast subscription and offloading the
//!   versioned-batch write to `spawn_blocking`. A `Lagged` receive means
//!   accepts were dropped — the index would gap — and is treated as fatal
//!   (17 §3).
//! - Restart markers match Go byte-for-byte: `hasRun = [0x07] → ""`,
//!   `previously-indexed = chainID ‖ 0x05 → ""`,
//!   `incomplete = chainID ‖ 0x04 → ""`, all written raw (unprefixed) into
//!   the indexer's DB.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use ava_database::{Database, PrefixDb};
use ava_snow::context::ConsensusContext;
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::{ID_LEN, Id};
use ava_utils::clock::Clock;

use crate::acceptor::{AcceptedContainer, AcceptorGroup};
use crate::error::Result;
use crate::index::{Index, IndexReader};
use crate::{Indexer, PathAdder, VmType, service};

/// Per-chain index prefix byte: transactions (Go `txPrefix`).
pub(crate) const TX_PREFIX: u8 = 0x01;
/// Per-chain index prefix byte: vertices (Go `vtxPrefix`).
pub(crate) const VTX_PREFIX: u8 = 0x02;
/// Per-chain index prefix byte: blocks (Go `blockPrefix`).
pub(crate) const BLOCK_PREFIX: u8 = 0x03;
/// Marker suffix: this chain's index is incomplete (Go `isIncompletePrefix`).
pub(crate) const IS_INCOMPLETE_PREFIX: u8 = 0x04;
/// Marker suffix: this chain was indexed in a previous run
/// (Go `previouslyIndexedPrefix`).
pub(crate) const PREVIOUSLY_INDEXED_PREFIX: u8 = 0x05;
/// Marker key: this node has run with this indexer DB before (Go `hasRunKey`).
pub(crate) const HAS_RUN_KEY: [u8; 1] = [0x07];

/// The shutdown callback fired (spawned, mirroring Go's `go i.shutdownF()`)
/// whenever the indexer closes — including fatal paths.
pub type ShutdownF = Arc<dyn Fn() + Send + Sync>;

/// Construction config (Go `indexer.Config`).
pub struct Config<D: Database> {
    /// The indexer's database (the node carves this out of its main DB).
    pub db: Arc<D>,
    /// `index-enabled` (12 §1.3 / M8.12).
    pub indexing_enabled: bool,
    /// `index-allow-incomplete`.
    pub allow_incomplete_index: bool,
    /// Fan-out of accepted blocks.
    pub block_acceptor_group: Arc<AcceptorGroup>,
    /// Fan-out of accepted transactions.
    pub tx_acceptor_group: Arc<AcceptorGroup>,
    /// Fan-out of accepted vertices.
    pub vertex_acceptor_group: Arc<AcceptorGroup>,
    /// Mounts `/ext/index/<alias>/<kind>` routes on the API server.
    pub path_adder: Arc<dyn PathAdder>,
    /// Fired on close (Go `ShutdownF`; the node's shutdown trigger).
    pub shutdown_f: ShutdownF,
    /// Injectable clock stamping accept times (Go `mockable.Clock`).
    pub clock: Arc<dyn Clock>,
}

/// The chain index a [`ContainerIndexer`] manages per (chain, kind).
pub type ChainIndex<D> = Arc<Index<PrefixDb<D>>>;

/// Everything the indexer owns; shared with the per-index acceptor tasks so
/// they can fatal-close the whole indexer.
struct Inner<D: Database + 'static> {
    db: Arc<D>,
    indexing_enabled: bool,
    allow_incomplete_index: bool,
    has_run_before: bool,
    clock: Arc<dyn Clock>,
    block_acceptor_group: Arc<AcceptorGroup>,
    tx_acceptor_group: Arc<AcceptorGroup>,
    vertex_acceptor_group: Arc<AcceptorGroup>,
    path_adder: Arc<dyn PathAdder>,
    shutdown_f: ShutdownF,
    /// Cancels every acceptor task on close.
    cancel: CancellationToken,
    state: Mutex<State<D>>,
}

/// The lock-guarded mutable state (Go guards the same fields with `i.lock`).
struct State<D: Database + 'static> {
    closed: bool,
    block_indices: HashMap<Id, ChainIndex<D>>,
    vtx_indices: HashMap<Id, ChainIndex<D>>,
    tx_indices: HashMap<Id, ChainIndex<D>>,
}

/// The indexer (Go `indexer.indexer`). Construct with
/// [`ContainerIndexer::new`]; drive through the [`Indexer`] trait (or the
/// `*_sync` inherent methods, which the trait wraps).
pub struct ContainerIndexer<D: Database + 'static> {
    inner: Arc<Inner<D>>,
}

impl<D: Database + 'static> ContainerIndexer<D> {
    /// Builds the indexer, reading then writing the `hasRun` marker
    /// (Go `NewIndexer`).
    ///
    /// # Errors
    /// Propagates database failures on the marker round-trip.
    pub fn new(config: Config<D>) -> Result<Self> {
        let has_run_before = config.db.has(&HAS_RUN_KEY)?;
        let inner = Arc::new(Inner {
            db: config.db,
            indexing_enabled: config.indexing_enabled,
            allow_incomplete_index: config.allow_incomplete_index,
            has_run_before,
            clock: config.clock,
            block_acceptor_group: config.block_acceptor_group,
            tx_acceptor_group: config.tx_acceptor_group,
            vertex_acceptor_group: config.vertex_acceptor_group,
            path_adder: config.path_adder,
            shutdown_f: config.shutdown_f,
            cancel: CancellationToken::new(),
            state: Mutex::new(State {
                closed: false,
                block_indices: HashMap::new(),
                vtx_indices: HashMap::new(),
                tx_indices: HashMap::new(),
            }),
        });
        // markHasRun: future runs know this DB has been used.
        inner.db.put(&HAS_RUN_KEY, &[])?;
        Ok(Self { inner })
    }

    /// Whether this DB was used by a previous run (Go `hasRunBefore`).
    #[must_use]
    pub fn has_run_before(&self) -> bool {
        self.inner.has_run_before
    }

    /// Whether the indexer has been closed (normally or fatally).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().closed
    }

    /// The chain's block index, if registered.
    #[must_use]
    pub fn block_index(&self, chain_id: &Id) -> Option<ChainIndex<D>> {
        self.inner.state.lock().block_indices.get(chain_id).cloned()
    }

    /// The chain's vertex index, if registered (DAG chains only).
    #[must_use]
    pub fn vtx_index(&self, chain_id: &Id) -> Option<ChainIndex<D>> {
        self.inner.state.lock().vtx_indices.get(chain_id).cloned()
    }

    /// The chain's transaction index, if registered (DAG chains only).
    #[must_use]
    pub fn tx_index(&self, chain_id: &Id) -> Option<ChainIndex<D>> {
        self.inner.state.lock().tx_indices.get(chain_id).cloned()
    }

    /// Whether the chain's index is marked incomplete (Go `isIncomplete`).
    ///
    /// # Errors
    /// Propagates database failures.
    pub fn is_incomplete(&self, chain_id: &Id) -> Result<bool> {
        self.inner.is_incomplete(chain_id)
    }

    /// Whether the chain was indexed in a previous run
    /// (Go `previouslyIndexed`).
    ///
    /// # Errors
    /// Propagates database failures.
    pub fn previously_indexed(&self, chain_id: &Id) -> Result<bool> {
        self.inner.previously_indexed(chain_id)
    }

    /// Synchronous core of [`Indexer::register_chain`] (Go `RegisterChain`).
    /// Must run inside a tokio runtime (it spawns the acceptor tasks).
    pub fn register_chain_sync(&self, chain_name: &str, ctx: &ConsensusContext, vm_type: VmType) {
        let inner = &self.inner;
        let mut state = inner.state.lock();

        if state.closed {
            tracing::debug!(chain_name, "not registering chain to indexer: closed");
            return;
        }
        if ctx.chain.subnet_id != PRIMARY_NETWORK_ID {
            tracing::debug!(
                chain_name,
                "not registering chain to indexer: not in the primary network"
            );
            return;
        }
        let chain_id = ctx.chain.chain_id;
        if state.block_indices.contains_key(&chain_id)
            || state.tx_indices.contains_key(&chain_id)
            || state.vtx_indices.contains_key(&chain_id)
        {
            tracing::warn!(%chain_id, "chain is already being indexed");
            return;
        }

        // If the index is incomplete, make sure that's OK. Otherwise die.
        let is_incomplete = match inner.is_incomplete(&chain_id) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(chain_name, error = %e, "couldn't get whether chain is incomplete");
                inner.close_logged(&mut state);
                return;
            }
        };
        let previously_indexed = match inner.previously_indexed(&chain_id) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(chain_name, error = %e, "couldn't get whether chain was previously indexed");
                inner.close_logged(&mut state);
                return;
            }
        };

        if !inner.indexing_enabled {
            if previously_indexed && !inner.allow_incomplete_index {
                // Indexed before but not this run: the index would gap.
                tracing::error!(
                    chain_name,
                    "FATAL: running would cause index to become incomplete but incomplete indices are disabled"
                );
                inner.close_logged(&mut state);
                return;
            }
            // Creating an incomplete index is allowed; mark it.
            if let Err(e) = inner.mark_incomplete(&chain_id) {
                tracing::error!(chain_name, error = %e, "FATAL: couldn't mark chain as incomplete");
                inner.close_logged(&mut state);
            }
            return;
        }

        if !inner.allow_incomplete_index
            && is_incomplete
            && (previously_indexed || inner.has_run_before)
        {
            tracing::error!(
                chain_name,
                "FATAL: index is incomplete but incomplete indices are disabled. Shutting down"
            );
            inner.close_logged(&mut state);
            return;
        }

        // Mark that in this run, this chain was indexed.
        if let Err(e) = inner.mark_previously_indexed(&chain_id) {
            tracing::error!(chain_name, error = %e, "couldn't mark chain as indexed");
            inner.close_logged(&mut state);
            return;
        }

        let block_index = match Self::register_chain_helper(
            inner,
            chain_id,
            BLOCK_PREFIX,
            chain_name,
            "block",
            &inner.block_acceptor_group,
        ) {
            Ok(index) => index,
            Err(e) => {
                tracing::error!(chain_name, endpoint = "block", error = %e, "FATAL: failed to create index");
                inner.close_logged(&mut state);
                return;
            }
        };
        state.block_indices.insert(chain_id, block_index);

        if vm_type == VmType::Dag {
            let vtx_index = match Self::register_chain_helper(
                inner,
                chain_id,
                VTX_PREFIX,
                chain_name,
                "vtx",
                &inner.vertex_acceptor_group,
            ) {
                Ok(index) => index,
                Err(e) => {
                    tracing::error!(chain_name, endpoint = "vtx", error = %e, "FATAL: couldn't create index");
                    inner.close_logged(&mut state);
                    return;
                }
            };
            state.vtx_indices.insert(chain_id, vtx_index);

            let tx_index = match Self::register_chain_helper(
                inner,
                chain_id,
                TX_PREFIX,
                chain_name,
                "tx",
                &inner.tx_acceptor_group,
            ) {
                Ok(index) => index,
                Err(e) => {
                    tracing::error!(chain_name, endpoint = "tx", error = %e, "FATAL: couldn't create index");
                    inner.close_logged(&mut state);
                    return;
                }
            };
            state.tx_indices.insert(chain_id, tx_index);
        }
    }

    /// Creates one index (`chainID ‖ kind` prefix), spawns its acceptor task,
    /// and mounts its API route (Go `registerChainHelper`).
    fn register_chain_helper(
        inner: &Arc<Inner<D>>,
        chain_id: Id,
        kind: u8,
        name: &str,
        endpoint: &str,
        group: &Arc<AcceptorGroup>,
    ) -> Result<ChainIndex<D>> {
        let mut prefix = Vec::with_capacity(ID_LEN.saturating_add(1));
        prefix.extend_from_slice(chain_id.as_bytes());
        prefix.push(kind);
        let index_db = Arc::new(PrefixDb::new_arc(&prefix, Arc::clone(&inner.db)));
        let index = Arc::new(Index::new(index_db, Arc::clone(&inner.clock))?);

        // Register for accepts BEFORE mounting the API (Go order). The task
        // owns the broadcast receiver; the index would gap if it lagged.
        let receiver = group.subscribe(chain_id);
        tokio::spawn(run_index_acceptor(
            Arc::clone(inner),
            Arc::clone(&index),
            receiver,
            endpoint.to_string(),
        ));

        // Mount /ext/index/<alias>/<endpoint> (14 §7).
        let handler = service::index_handler(Arc::clone(&index) as Arc<dyn IndexReader>);
        if let Err(e) =
            inner
                .path_adder
                .add_route(handler, &format!("index/{name}"), &format!("/{endpoint}"))
        {
            let _ = index.close();
            return Err(crate::Error::Route(e.to_string()));
        }
        Ok(index)
    }

    /// Synchronous core of [`Indexer::close`]; idempotent (Go `Close`).
    /// Must run inside a tokio runtime (it spawns the shutdown callback).
    ///
    /// # Errors
    /// Returns the first close error after attempting every close.
    pub fn close_sync(&self) -> Result<()> {
        let mut state = self.inner.state.lock();
        self.inner.close_locked(&mut state)
    }
}

impl<D: Database + 'static> Inner<D> {
    /// `chainID ‖ marker` (Go builds the same 33-byte key).
    fn marker_key(chain_id: &Id, marker: u8) -> Vec<u8> {
        let mut key = Vec::with_capacity(ID_LEN.saturating_add(1));
        key.extend_from_slice(chain_id.as_bytes());
        key.push(marker);
        key
    }

    fn mark_incomplete(&self, chain_id: &Id) -> Result<()> {
        Ok(self
            .db
            .put(&Self::marker_key(chain_id, IS_INCOMPLETE_PREFIX), &[])?)
    }

    fn is_incomplete(&self, chain_id: &Id) -> Result<bool> {
        Ok(self
            .db
            .has(&Self::marker_key(chain_id, IS_INCOMPLETE_PREFIX))?)
    }

    fn mark_previously_indexed(&self, chain_id: &Id) -> Result<()> {
        Ok(self
            .db
            .put(&Self::marker_key(chain_id, PREVIOUSLY_INDEXED_PREFIX), &[])?)
    }

    fn previously_indexed(&self, chain_id: &Id) -> Result<bool> {
        Ok(self
            .db
            .has(&Self::marker_key(chain_id, PREVIOUSLY_INDEXED_PREFIX))?)
    }

    /// Fatal-close from an acceptor task (takes the state lock itself).
    fn close(&self) -> Result<()> {
        let mut state = self.state.lock();
        self.close_locked(&mut state)
    }

    /// Fatal-close on the registration path: Go logs (rather than returns)
    /// the close error after a `log.Fatal`.
    fn close_logged(&self, state: &mut State<D>) {
        if let Err(e) = self.close_locked(state) {
            tracing::error!(error = %e, "failed to close indexer");
        }
    }

    /// Close everything once; later calls do nothing (Go `close`). Cancels
    /// the acceptor tasks, closes every index and the DB, and spawns the
    /// shutdown callback (Go `go i.shutdownF()`).
    fn close_locked(&self, state: &mut State<D>) -> Result<()> {
        if state.closed {
            return Ok(());
        }
        state.closed = true;
        self.cancel.cancel();

        let mut first_err: Option<crate::Error> = None;
        let mut record = |r: Result<()>| {
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        };
        for index in state.tx_indices.values() {
            record(index.close());
        }
        for index in state.vtx_indices.values() {
            record(index.close());
        }
        for index in state.block_indices.values() {
            record(index.close());
        }
        record(Database::close(&*self.db).map_err(crate::Error::from));

        let shutdown_f = Arc::clone(&self.shutdown_f);
        tokio::spawn(async move { shutdown_f() });

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// The per-index acceptor task (17 §2.2 #20): drains the broadcast
/// subscription in acceptance order and offloads each versioned-batch write
/// to `spawn_blocking`. Any write error — and a `Lagged` receive, which means
/// the index would gap (17 §3) — is fatal to the whole indexer. A write that
/// fails only because shutdown was already initiated is a normal close.
async fn run_index_acceptor<D: Database + 'static>(
    inner: Arc<Inner<D>>,
    index: Arc<Index<PrefixDb<D>>>,
    mut receiver: broadcast::Receiver<AcceptedContainer>,
    endpoint: String,
) {
    let cancel = inner.cancel.clone();
    loop {
        let message = tokio::select! {
            () = cancel.cancelled() => return,
            message = receiver.recv() => message,
        };
        match message {
            Ok(accepted) => {
                let write_index = Arc::clone(&index);
                let result = tokio::task::spawn_blocking(move || {
                    write_index.accept(accepted.container_id, &accepted.bytes)
                })
                .await;
                let fatal = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(e.to_string()),
                };
                if let Some(error) = fatal {
                    // A write that failed because shutdown was already
                    // initiated (close cancels the tasks and closes the
                    // indices underneath any in-flight `spawn_blocking`
                    // write) is a normal close, not a fault: return quietly.
                    if cancel.is_cancelled() {
                        return;
                    }
                    tracing::error!(endpoint, error, "FATAL: failed to index accepted container");
                    if let Err(e) = inner.close() {
                        tracing::error!(error = %e, "failed to close indexer");
                    }
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::error!(
                    endpoint,
                    missed,
                    "FATAL: indexer lagged behind accepted containers; index would be incomplete"
                );
                if let Err(e) = inner.close() {
                    tracing::error!(error = %e, "failed to close indexer");
                }
                return;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[async_trait]
impl<D: Database + 'static> Indexer for ContainerIndexer<D> {
    async fn register_chain(&self, chain_name: &str, ctx: &ConsensusContext, vm_type: VmType) {
        self.register_chain_sync(chain_name, ctx, vm_type);
    }

    async fn close(&self) -> Result<()> {
        self.close_sync()
    }
}

#[cfg(test)]
// Tests index into fixtures and `serde_json::Value` replies and do plain
// test-fixture arithmetic (`UNIX_EPOCH + ...`), both idiomatic in tests
// (precedent: ava-api jsonrpc tests).
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use ava_api::BoxedHandler;
    use ava_database::memdb::MemDb;
    use ava_database::{Database, VersionDb};
    use ava_snow::acceptor::NoOpAcceptor;
    use ava_snow::context::{ChainContext, ConsensusContext};
    use ava_types::constants::PRIMARY_NETWORK_ID;
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_utils::clock::MockClock;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use tokio::sync::Notify;

    use super::*;
    use crate::acceptor::AcceptorGroup;
    use crate::{PathAdder, VmType};

    /// Mirrors Go's `apiServerMock`: records (base, endpoint) pairs.
    #[derive(Default)]
    struct PathRecorder {
        routes: Mutex<Vec<(String, String)>>,
    }

    impl PathAdder for PathRecorder {
        fn add_route(
            &self,
            _handler: BoxedHandler,
            base: &str,
            endpoint: &str,
        ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.routes
                .lock()
                .push((base.to_string(), endpoint.to_string()));
            Ok(())
        }
    }

    struct Harness {
        config_db: Arc<VersionDb<MemDb>>,
        recorder: Arc<PathRecorder>,
        shutdown: Arc<Notify>,
        block_group: Arc<AcceptorGroup>,
        tx_group: Arc<AcceptorGroup>,
        vertex_group: Arc<AcceptorGroup>,
    }

    fn harness(base: &Arc<MemDb>) -> Harness {
        Harness {
            config_db: Arc::new(VersionDb::new_arc(Arc::clone(base))),
            recorder: Arc::new(PathRecorder::default()),
            shutdown: Arc::new(Notify::new()),
            block_group: Arc::new(AcceptorGroup::default()),
            tx_group: Arc::new(AcceptorGroup::default()),
            vertex_group: Arc::new(AcceptorGroup::default()),
        }
    }

    fn config(
        h: &Harness,
        indexing_enabled: bool,
        allow_incomplete: bool,
    ) -> Config<VersionDb<MemDb>> {
        let shutdown = Arc::clone(&h.shutdown);
        Config {
            db: Arc::clone(&h.config_db),
            indexing_enabled,
            allow_incomplete_index: allow_incomplete,
            block_acceptor_group: Arc::clone(&h.block_group),
            tx_acceptor_group: Arc::clone(&h.tx_group),
            vertex_acceptor_group: Arc::clone(&h.vertex_group),
            path_adder: Arc::clone(&h.recorder) as Arc<dyn PathAdder>,
            shutdown_f: Arc::new(move || shutdown.notify_one()),
            clock: Arc::new(MockClock::at(std::time::UNIX_EPOCH)),
        }
    }

    fn consensus_ctx(chain_id: Id, subnet_id: Id, alias: &str) -> ConsensusContext {
        ConsensusContext::new(
            Arc::new(ChainContext {
                network_id: 1,
                subnet_id,
                chain_id,
                node_id: NodeId::default(),
                public_key: None,
                network_upgrades: ava_version::upgrade::get_config(1),
                x_chain_id: Id::EMPTY,
                c_chain_id: Id::EMPTY,
                avax_asset_id: Id::EMPTY,
                chain_data_dir: PathBuf::new(),
            }),
            alias.to_string(),
            Arc::new(NoOpAcceptor),
            Arc::new(NoOpAcceptor),
        )
    }

    /// Polls until `id` is indexed (the acceptor path is async in Rust —
    /// broadcast + spawn_blocking, 17 §2.2 #20).
    async fn wait_indexed<D: Database + 'static>(index: &crate::Index<D>, id: &Id) {
        for _ in 0..1000 {
            if index.get_index(id).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("container {id} was not indexed in time");
    }

    // hasRun marker (key 0x07) is written on construction; the shutdown
    // callback fires on close (Go TestNewIndexer + TestMarkHasRunAndShutdown).
    #[tokio::test]
    async fn has_run_marker_and_shutdown() {
        let base = Arc::new(MemDb::new());
        let h = harness(&base);
        let indexer =
            ContainerIndexer::new(config(&h, true, false)).expect("ContainerIndexer::new()");
        assert!(!indexer.has_run_before(), "first run");
        assert!(!indexer.is_closed(), "open after construction");
        h.config_db.commit().expect("VersionDb::commit()");
        indexer.close_sync().expect("close()");
        assert!(indexer.is_closed(), "closed flag");
        tokio::time::timeout(Duration::from_secs(5), h.shutdown.notified())
            .await
            .expect("shutdown_f fired on close");
        // Idempotent close.
        indexer.close_sync().expect("close() again");

        let h2 = harness(&base);
        let indexer = ContainerIndexer::new(config(&h2, true, false)).expect("reopen");
        assert!(indexer.has_run_before(), "hasRun persisted");
        indexer.close_sync().expect("close()");
    }

    // Full register/accept/restart flow (Go TestIndexer): routes mounted at
    // index/<alias>/{block,vtx,tx}; accepts flow from the AcceptorGroups into
    // the per-chain indices; restart markers keep state.
    #[tokio::test]
    async fn register_chain_and_accept() {
        let base = Arc::new(MemDb::new());
        let chain1 = Id::from([0xC1; 32]);
        let chain2 = Id::from([0xC2; 32]);

        let h = harness(&base);
        let indexer =
            ContainerIndexer::new(config(&h, true, false)).expect("ContainerIndexer::new()");

        // Snowman chain: one block index, one route.
        let ctx1 = consensus_ctx(chain1, PRIMARY_NETWORK_ID, "chain1");
        indexer.register_chain_sync("chain1", &ctx1, VmType::Chain);
        assert!(
            indexer
                .previously_indexed(&chain1)
                .expect("previously_indexed()"),
            "chain1 marked previously indexed"
        );
        assert!(
            !indexer.is_incomplete(&chain1).expect("is_incomplete()"),
            "chain1 not incomplete"
        );
        assert_eq!(
            vec![("index/chain1".to_string(), "/block".to_string())],
            h.recorder.routes.lock().clone(),
            "block route mounted"
        );

        // Re-registering the same chain is a warn-and-skip.
        indexer.register_chain_sync("chain1", &ctx1, VmType::Chain);
        assert_eq!(1, h.recorder.routes.lock().len(), "no duplicate routes");

        // Accept a block through the group; the async task indexes it.
        let blk_id = Id::from([0xB1; 32]);
        h.block_group.accept(&chain1, blk_id, &[1, 2, 3]);
        let blk_index = indexer.block_index(&chain1).expect("block index exists");
        wait_indexed(&blk_index, &blk_id).await;
        assert_eq!(
            vec![1, 2, 3],
            blk_index
                .get_container_by_id(&blk_id)
                .expect("get_container_by_id()")
                .bytes,
            "indexed block bytes"
        );

        // DAG chain: block + vtx + tx indices and routes.
        let ctx2 = consensus_ctx(chain2, PRIMARY_NETWORK_ID, "chain2");
        indexer.register_chain_sync("chain2", &ctx2, VmType::Dag);
        {
            let routes = h.recorder.routes.lock();
            assert_eq!(4, routes.len(), "block + dag(block,vtx,tx) routes");
            assert!(routes.contains(&("index/chain2".to_string(), "/block".to_string())));
            assert!(routes.contains(&("index/chain2".to_string(), "/vtx".to_string())));
            assert!(routes.contains(&("index/chain2".to_string(), "/tx".to_string())));
        }

        let vtx_id = Id::from([0xD1; 32]);
        h.vertex_group.accept(&chain2, vtx_id, &[4, 5]);
        let vtx_index = indexer.vtx_index(&chain2).expect("vtx index exists");
        wait_indexed(&vtx_index, &vtx_id).await;

        let tx_id = Id::from([0xE1; 32]);
        h.tx_group.accept(&chain2, tx_id, &[6]);
        let tx_index = indexer.tx_index(&chain2).expect("tx index exists");
        wait_indexed(&tx_index, &tx_id).await;

        // Cross-index isolation: each index has exactly its own container.
        assert_eq!(
            blk_id,
            blk_index.get_last_accepted().expect("blk last").id,
            "block index isolated"
        );
        assert_eq!(
            vtx_id,
            vtx_index.get_last_accepted().expect("vtx last").id,
            "vtx index isolated"
        );

        // Restart: state survives.
        h.config_db.commit().expect("commit");
        indexer.close_sync().expect("close()");

        let h2 = harness(&base);
        let indexer = ContainerIndexer::new(config(&h2, true, false)).expect("reopen");
        assert!(indexer.has_run_before(), "hasRun persisted");
        indexer.register_chain_sync(
            "chain1",
            &consensus_ctx(chain1, PRIMARY_NETWORK_ID, "chain1"),
            VmType::Chain,
        );
        indexer.register_chain_sync(
            "chain2",
            &consensus_ctx(chain2, PRIMARY_NETWORK_ID, "chain2"),
            VmType::Dag,
        );
        assert!(!indexer.is_closed(), "reopen with complete index is fine");
        assert_eq!(
            blk_id,
            indexer
                .block_index(&chain1)
                .expect("block index")
                .get_last_accepted()
                .expect("last accepted after restart")
                .id,
            "block index state restored"
        );
        assert_eq!(
            tx_id,
            indexer
                .tx_index(&chain2)
                .expect("tx index")
                .get_last_accepted()
                .expect("tx last accepted after restart")
                .id,
            "tx index state restored"
        );
        indexer.close_sync().expect("close()");
    }

    // ------------------------------------------------------------------
    // Red (M8.24): toggling index-enabled such that an index would gap with
    // index-allow-incomplete=false is FATAL — the indexer closes itself and
    // fires shutdown_f (Go TestIncompleteIndex; 12 §5).
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn incomplete_index_fatal() {
        let base = Arc::new(MemDb::new());
        let chain1 = Id::from([0xC1; 32]);
        let ctx = || consensus_ctx(chain1, PRIMARY_NETWORK_ID, "chain1");

        // Run 1: indexing disabled, incomplete not allowed, chain never
        // indexed before -> marked incomplete (not fatal).
        let h = harness(&base);
        let indexer = ContainerIndexer::new(config(&h, false, false)).expect("run 1");
        indexer.register_chain_sync("chain1", &ctx(), VmType::Chain);
        assert!(
            indexer.is_incomplete(&chain1).expect("is_incomplete()"),
            "chain marked incomplete when indexing disabled"
        );
        assert!(indexer.block_index(&chain1).is_none(), "no index created");
        assert!(!indexer.is_closed(), "marking incomplete is not fatal");
        h.config_db.commit().expect("commit");
        indexer.close_sync().expect("close()");

        // Run 2: indexing re-enabled with incomplete disallowed -> fatal.
        let h2 = harness(&base);
        let indexer = ContainerIndexer::new(config(&h2, true, false)).expect("run 2");
        indexer.register_chain_sync("chain1", &ctx(), VmType::Chain);
        assert!(
            indexer.is_closed(),
            "incomplete index with allow=false is fatal"
        );
        tokio::time::timeout(Duration::from_secs(5), h2.shutdown.notified())
            .await
            .expect("fatal fires shutdown_f");
        indexer.close_sync().expect("close() after fatal");

        // Run 3: incomplete allowed -> OK.
        let h3 = harness(&base);
        let indexer = ContainerIndexer::new(config(&h3, true, true)).expect("run 3");
        indexer.register_chain_sync("chain1", &ctx(), VmType::Chain);
        assert!(!indexer.is_closed(), "incomplete allowed proceeds");
        h3.config_db.commit().expect("commit run 3");
        indexer.close_sync().expect("close()");

        // Run 4 (Go's disabled-after-indexed branch): a fresh chain indexed in
        // run 3 then disabled with allow=false -> fatal.
        let h4 = harness(&base);
        let indexer = ContainerIndexer::new(config(&h4, false, false)).expect("run 4");
        indexer.register_chain_sync("chain1", &ctx(), VmType::Chain);
        assert!(
            indexer.is_closed(),
            "disabling indexing for a previously indexed chain is fatal"
        );
    }

    // A Lagged receive means accepts were dropped — the index would gap — so
    // the acceptor task fatal-closes the whole indexer and shutdown_f fires
    // (17 §3). Deterministic: a capacity-1 broadcast channel overflowed by
    // two sends lags the subscriber before the task ever polls it.
    #[tokio::test]
    async fn lagged_receiver_is_fatal() {
        let base = Arc::new(MemDb::new());
        let chain1 = Id::from([0xC1; 32]);
        let h = harness(&base);
        let indexer =
            ContainerIndexer::new(config(&h, true, false)).expect("ContainerIndexer::new()");
        indexer.register_chain_sync(
            "chain1",
            &consensus_ctx(chain1, PRIMARY_NETWORK_ID, "chain1"),
            VmType::Chain,
        );
        let index = indexer.block_index(&chain1).expect("block index exists");

        let (sender, receiver) = tokio::sync::broadcast::channel(1);
        for i in 0..2u8 {
            sender
                .send(crate::acceptor::AcceptedContainer {
                    container_id: Id::from([i; 32]),
                    bytes: Arc::from(&[i][..]),
                })
                .expect("broadcast::Sender::send()");
        }
        tokio::spawn(run_index_acceptor(
            Arc::clone(&indexer.inner),
            index,
            receiver,
            "block".to_string(),
        ))
        .await
        .expect("acceptor task join");

        assert!(
            indexer.is_closed(),
            "lagged receive fatal-closes the indexer"
        );
        tokio::time::timeout(Duration::from_secs(5), h.shutdown.notified())
            .await
            .expect("lagged fatal fires shutdown_f");
    }

    // A genuine write error with shutdown NOT initiated is fatal: the index's
    // DB is closed underneath it (without cancelling the indexer), so the
    // accept write fails, the indexer closes, and shutdown_f fires. This is
    // the counterpart of the quiet write-abort on graceful close (which only
    // triggers once `cancel` is already cancelled).
    #[tokio::test]
    async fn write_error_is_fatal() {
        let base = Arc::new(MemDb::new());
        let chain1 = Id::from([0xC1; 32]);
        let h = harness(&base);
        let indexer =
            ContainerIndexer::new(config(&h, true, false)).expect("ContainerIndexer::new()");
        indexer.register_chain_sync(
            "chain1",
            &consensus_ctx(chain1, PRIMARY_NETWORK_ID, "chain1"),
            VmType::Chain,
        );
        let index = indexer.block_index(&chain1).expect("block index exists");

        // Break the index without initiating shutdown.
        index.close().expect("Index::close()");
        assert!(
            !indexer.inner.cancel.is_cancelled(),
            "shutdown not initiated before the write error"
        );

        let (sender, receiver) = tokio::sync::broadcast::channel(1);
        sender
            .send(crate::acceptor::AcceptedContainer {
                container_id: Id::from([0xB1; 32]),
                bytes: Arc::from(&[1u8, 2, 3][..]),
            })
            .expect("broadcast::Sender::send()");
        tokio::spawn(run_index_acceptor(
            Arc::clone(&indexer.inner),
            index,
            receiver,
            "block".to_string(),
        ))
        .await
        .expect("acceptor task join");

        assert!(indexer.is_closed(), "write error fatal-closes the indexer");
        tokio::time::timeout(Duration::from_secs(5), h.shutdown.notified())
            .await
            .expect("write-error fatal fires shutdown_f");
    }

    // Non-Primary-Network chains are skipped (Go TestIgnoreNonDefaultChains).
    #[tokio::test]
    async fn ignore_non_primary_chains() {
        let base = Arc::new(MemDb::new());
        let h = harness(&base);
        let indexer =
            ContainerIndexer::new(config(&h, true, false)).expect("ContainerIndexer::new()");
        let ctx = consensus_ctx(Id::from([0xC9; 32]), Id::from([0x55; 32]), "subnet-chain");
        indexer.register_chain_sync("subnet-chain", &ctx, VmType::Chain);
        assert!(
            indexer.block_index(&Id::from([0xC9; 32])).is_none(),
            "no index"
        );
        assert!(h.recorder.routes.lock().is_empty(), "no routes");
        indexer.close_sync().expect("close()");
    }
}
