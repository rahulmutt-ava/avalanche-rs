// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 17 (specs/12 §2.2): the Block / Tx / Vertex `AcceptorGroup`s
//! (mirror Go `initEventDispatchers`).

use std::sync::Arc;

use ava_indexer::acceptor::AcceptorGroup;

/// Per-chain broadcast capacity for accepted-container fan-out. Go's
/// `AcceptorGroup` fan-out is synchronous (unbounded); the Rust groups are
/// `tokio::broadcast` channels — 1024 undelivered accepts before a slow
/// subscriber lags (the indexer treats `Lagged` as fatal, M8.24).
const ACCEPTOR_CAPACITY: usize = 1024;

/// The node's event dispatchers (Go `n.BlockAcceptorGroup` etc.).
pub struct EventDispatchers {
    /// Accepted snowman blocks.
    pub block: Arc<AcceptorGroup>,
    /// Accepted DAG transactions.
    pub tx: Arc<AcceptorGroup>,
    /// Accepted DAG vertices.
    pub vertex: Arc<AcceptorGroup>,
}

/// Build the three groups (mirror Go `initEventDispatchers`).
#[must_use]
pub fn init_event_dispatchers() -> EventDispatchers {
    EventDispatchers {
        block: Arc::new(AcceptorGroup::new(ACCEPTOR_CAPACITY)),
        tx: Arc::new(AcceptorGroup::new(ACCEPTOR_CAPACITY)),
        vertex: Arc::new(AcceptorGroup::new(ACCEPTOR_CAPACITY)),
    }
}
