// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The async-VM-block → sync-consensus-block adaptor (specs 06 §2.4 / §3.1).
//!
//! `ava-snow`'s consensus core ([`SnowmanConsensus`](ava_snow::snowman::SnowmanConsensus))
//! decides `accept`/`reject` **synchronously** (Go threads them directly inside
//! `RecordPoll`), but the engine's `block.ChainVM` blocks
//! ([`ava_vm::Block`]) expose **async** `verify`/`accept`/`reject`.
//! [`BlockAdaptor`] wraps an `Arc<dyn ava_vm::Block>` and implements the
//! synchronous [`ava_snow::snowman::block::Block`] by driving the async
//! accept/reject to completion with [`futures::executor::block_on`].
//!
//! Using `futures`' standalone executor (rather than `tokio::runtime::Handle::
//! block_on` or `block_in_place`) means the adaptor never re-enters the engine's
//! tokio runtime, so it works on the current-thread test runtime
//! (`#[tokio::test(start_paused = true)]`) without panicking. This is sound for
//! VMs whose `accept`/`reject` perform no tokio-driven I/O (e.g. the in-memory
//! test VM). M3.14+ revisits the bridge for production VMs that await tokio I/O
//! inside accept (those will move acceptance off the synchronous `record_poll`
//! path). See `tests/PORTING.md`.

use std::sync::Arc;
use std::time::SystemTime;

use tokio_util::sync::CancellationToken;

use ava_snow::error::Result;
use ava_snow::snowman::block::Block as ConsensusBlock;
use ava_types::id::Id;
use ava_vm::Block as VmBlock;

/// Wraps an async [`ava_vm::Block`] as a synchronous consensus
/// [`Block`](ConsensusBlock).
pub struct BlockAdaptor {
    inner: Arc<dyn VmBlock>,
    token: CancellationToken,
}

impl BlockAdaptor {
    /// Wraps `inner`, carrying `token` so the bridged accept/reject observe the
    /// engine halt signal.
    #[must_use]
    pub fn new(inner: Arc<dyn VmBlock>, token: CancellationToken) -> Self {
        Self { inner, token }
    }

    /// The wrapped VM block.
    #[must_use]
    pub fn inner(&self) -> &Arc<dyn VmBlock> {
        &self.inner
    }
}

impl ConsensusBlock for BlockAdaptor {
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
        // The token is threaded into the async accept; the VM observes it for
        // prompt halt. The bridge itself does not re-enter the tokio runtime.
        futures::executor::block_on(self.inner.accept(&self.token))
    }

    fn reject(&self) -> Result<()> {
        futures::executor::block_on(self.inner.reject(&self.token))
    }
}
