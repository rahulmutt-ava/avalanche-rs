// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Accept-callback trait (specs 06 §3.1; Go `snow/acceptor.go`).

use async_trait::async_trait;

use ava_types::id::Id;

use crate::context::ConsensusContext;
use crate::error::Result;

/// Fired when a container (block/tx/vertex) is accepted by consensus.
///
/// `Topological` invokes the block acceptor **before** the VM block `accept`
/// (specs 06 §2.4 ordering invariant): the acceptor notifies indexers /
/// atomic-memory, then the VM commits state.
#[async_trait]
pub trait Acceptor: Send + Sync {
    /// Called with the accepted container's id and serialized bytes.
    async fn accept(&self, ctx: &ConsensusContext, container_id: Id, bytes: &[u8]) -> Result<()>;
}

/// An [`Acceptor`] that does nothing, used where no callback is wired (tests and
/// chains without indexing). Mirrors Go's no-op acceptor.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpAcceptor;

#[async_trait]
impl Acceptor for NoOpAcceptor {
    async fn accept(
        &self,
        _ctx: &ConsensusContext,
        _container_id: Id,
        _bytes: &[u8],
    ) -> Result<()> {
        Ok(())
    }
}
