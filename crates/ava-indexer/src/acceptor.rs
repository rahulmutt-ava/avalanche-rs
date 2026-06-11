// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Broadcast-based acceptor fan-out (Go `snow.AcceptorGroup`; specs 17 §2.2
//! #20 / §3).
//!
//! Go's `AcceptorGroup` invokes registered acceptors synchronously inside the
//! accept path. The Rust mapping (17 §3) replaces the callback fan-out with a
//! per-chain `tokio::sync::broadcast` channel: the engine-side
//! [`ava_snow::acceptor::Acceptor`] impl publishes `(container id, bytes)` and
//! each subscriber (the indexer's per-index task, WS pub-sub, …) drains its own
//! receiver. A lagging receiver gets `RecvError::Lagged` — for the indexer that
//! means the index would gap, which is **fatal** (17 §3); see
//! `crate::indexer`.
//!
//! This seam lives here (not in `ava-snow`) until the node wires the acceptor
//! path in M8.29; `ava-snow` only carries the `Acceptor` callback trait.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::sync::broadcast;

use ava_snow::acceptor::Acceptor;
use ava_snow::context::ConsensusContext;
use ava_types::id::Id;

/// Default broadcast capacity per chain (17 §3: "tuned (e.g. 1024)").
pub const DEFAULT_ACCEPTOR_CAPACITY: usize = 1024;

/// One accepted container, fanned out to every subscriber of its chain.
#[derive(Clone, Debug)]
pub struct AcceptedContainer {
    /// The accepted container's id.
    pub container_id: Id,
    /// The container's serialized bytes (shared, cheap to clone per receiver).
    pub bytes: Arc<[u8]>,
}

/// Per-chain accepted-container fan-out (Go `snow.AcceptorGroup`).
///
/// The node owns three groups — block / tx / vertex — and the consensus side
/// publishes into them via the [`Acceptor`] impl; subscribers (indexer tasks)
/// call [`AcceptorGroup::subscribe`].
pub struct AcceptorGroup {
    capacity: usize,
    chains: RwLock<HashMap<Id, broadcast::Sender<AcceptedContainer>>>,
}

impl AcceptorGroup {
    /// A group whose per-chain channels hold up to `capacity` undelivered
    /// accepts before a slow receiver lags.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            chains: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribes to `chain_id`'s accepted containers, creating the channel if
    /// this is the first subscriber (Go `RegisterAcceptor`).
    pub fn subscribe(&self, chain_id: Id) -> broadcast::Receiver<AcceptedContainer> {
        let mut chains = self.chains.write();
        chains
            .entry(chain_id)
            .or_insert_with(|| broadcast::channel(self.capacity).0)
            .subscribe()
    }

    /// Publishes an accepted container to `chain_id`'s subscribers (Go
    /// `AcceptorGroup.Accept`). A chain with no subscribers is a no-op,
    /// matching Go's empty acceptor list.
    pub fn accept(&self, chain_id: &Id, container_id: Id, bytes: &[u8]) {
        let chains = self.chains.read();
        if let Some(sender) = chains.get(chain_id) {
            // `send` errs only when every receiver is gone (e.g. the indexer
            // closed); like Go's deregistered acceptor, the accept is dropped.
            let _ = sender.send(AcceptedContainer {
                container_id,
                bytes: Arc::from(bytes),
            });
        }
    }
}

impl Default for AcceptorGroup {
    fn default() -> Self {
        Self::new(DEFAULT_ACCEPTOR_CAPACITY)
    }
}

#[async_trait]
impl Acceptor for AcceptorGroup {
    /// The engine-side publish hook: a `ConsensusContext` acceptor callback
    /// that fans the accept out to this group's subscribers.
    async fn accept(
        &self,
        ctx: &ConsensusContext,
        container_id: Id,
        bytes: &[u8],
    ) -> ava_snow::error::Result<()> {
        Self::accept(self, &ctx.chain.chain_id, container_id, bytes);
        Ok(())
    }
}
