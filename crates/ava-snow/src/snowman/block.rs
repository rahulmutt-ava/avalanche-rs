// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman consensus [`Block`] trait and the [`BlockAcceptor`] callback
//! (specs 06 §2.4; Go `snow/consensus/snowman/block.go`, `snow/acceptor.go`).
//!
//! These are the **synchronous** consensus-internal interfaces, distinct from
//! the crate's engine-facing async [`Block`](crate::decidable::Block). Snowman
//! decisions (accept/reject) are synchronous and fallible in Go (`Accept(ctx)
//! error`); the consensus core threads them directly, so the trait here is
//! synchronous and returns [`Result`]. The async adaptor that bridges the
//! engine's `block.ChainVM` to this trait lands with the engine (M3.14+).

use std::time::SystemTime;

use ava_types::id::Id;

use crate::error::Result;

/// A Snowman block as seen by the consensus core: a linearly-ordered, decidable
/// container whose accept/reject are synchronous and fallible.
pub trait Block: Send + Sync {
    /// The unique identifier of this block.
    fn id(&self) -> Id;

    /// The identifier of this block's parent.
    fn parent(&self) -> Id;

    /// The height of this block in the chain.
    fn height(&self) -> u64;

    /// The block's timestamp.
    fn timestamp(&self) -> SystemTime;

    /// The canonical serialized bytes of this block.
    fn bytes(&self) -> &[u8];

    /// Commits this block to the chain.
    ///
    /// # Errors
    /// Propagates any VM-side acceptance error; a returned `Err` is critical and
    /// halts the chain (matching Go).
    fn accept(&self) -> Result<()>;

    /// Discards this block from the chain.
    ///
    /// # Errors
    /// Propagates any VM-side rejection error; a returned `Err` is critical.
    fn reject(&self) -> Result<()>;
}

/// Fired when a block is accepted by consensus, **before** the block's own
/// `accept` (Go `Acceptor.Accept` invariant; specs 06 §2.4).
pub trait BlockAcceptor: Send + Sync {
    /// Notifies that `container_id` (with the given `bytes`) was accepted.
    ///
    /// # Errors
    /// A returned `Err` aborts the acceptance and halts the chain.
    fn accept(&self, container_id: Id, bytes: &[u8]) -> Result<()>;
}

/// A [`BlockAcceptor`] that does nothing (tests / chains without indexing).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpBlockAcceptor;

impl BlockAcceptor for NoOpBlockAcceptor {
    fn accept(&self, _container_id: Id, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }
}
