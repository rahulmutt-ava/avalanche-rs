// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `block.ChainVM` — the Snowman VM (specs 07 §2.4; Go
//! `snow/engine/snowman/block/vm.go`), plus the `*WithContext` capability
//! traits.
//!
//! The engine (06) holds the per-chain VM and is its only caller, so the trait
//! takes `&mut self` only for the genuinely mutating ops (`build_block`,
//! `set_preference`) and `&self` for the read ops. Optional capabilities
//! (`BuildBlockWithContextChainVM`, `SetPreferenceWithContextChainVM`,
//! `BatchedChainVM`, `StateSyncableVM`) are probed via `as_*` accessors that
//! default to `None`, mirroring Go's interface type-assertions.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::block::batched::BatchedChainVm;
use crate::block::state_sync::StateSyncableVm;
use crate::block::with_context::BlockContext;
use crate::block::Block;
use crate::error::Result;
use crate::vm::Vm;

/// `block.ChainVM` — the base Snowman VM the consensus engine drives.
#[async_trait]
pub trait ChainVm: Vm {
    /// `BuildBlock` — build a block on top of the currently preferred block.
    /// `Err` if the VM does not want to issue a block.
    async fn build_block(&mut self, token: &CancellationToken) -> Result<Arc<dyn Block>>;

    /// `Getter.GetBlock` — fetch a block by id. `Err(Error::NotFound)` if
    /// unknown.
    async fn get_block(&self, token: &CancellationToken, id: Id) -> Result<Arc<dyn Block>>;

    /// `Parser.ParseBlock` — parse a block from its bytes. The bytes must
    /// round-trip to the same block on every node.
    async fn parse_block(&self, token: &CancellationToken, bytes: &[u8])
        -> Result<Arc<dyn Block>>;

    /// `SetPreference` — set the engine's currently preferred (leaf) block.
    async fn set_preference(&mut self, token: &CancellationToken, id: Id) -> Result<()>;

    /// `LastAccepted` — the id of the last accepted block (genesis if nothing
    /// has been accepted yet).
    async fn last_accepted(&self, token: &CancellationToken) -> Result<Id>;

    /// `GetBlockIDAtHeight` — the accepted block id at `height`.
    /// `Err(Error::NotFound)` if the height index is unknown (e.g. after state
    /// sync pruned it).
    async fn get_block_id_at_height(&self, token: &CancellationToken, height: u64) -> Result<Id>;

    // ---- optional capability probes (Go: interface type-assertions) ----

    /// `BuildBlockWithContextChainVM`. Probed by the engine; called iff
    /// proposervm is active. Defaults to unsupported.
    fn as_build_with_context(&self) -> Option<&dyn BuildBlockWithContext> {
        None
    }

    /// `SetPreferenceWithContextChainVM`. Defaults to unsupported.
    fn as_set_preference_with_context(&self) -> Option<&dyn SetPreferenceWithContext> {
        None
    }

    /// `BatchedChainVM`. Defaults to unsupported (the `get_ancestors` /
    /// `batched_parse_block` fallbacks handle this case).
    fn as_batched(&self) -> Option<&dyn BatchedChainVm> {
        None
    }

    /// `StateSyncableVM`. Defaults to unsupported.
    fn as_state_syncable(&self) -> Option<&dyn StateSyncableVm> {
        None
    }
}

/// `block.BuildBlockWithContextChainVM` — build a block against a
/// [`BlockContext`] (proposervm-driven).
#[async_trait]
pub trait BuildBlockWithContext: Send + Sync {
    /// `BuildBlockWithContext`.
    async fn build_block_with_context(
        &self,
        token: &CancellationToken,
        ctx: &BlockContext,
    ) -> Result<Arc<dyn Block>>;
}

/// `block.SetPreferenceWithContextChainVM` — set preference against a
/// [`BlockContext`].
#[async_trait]
pub trait SetPreferenceWithContext: Send + Sync {
    /// `SetPreferenceWithContext`.
    async fn set_preference_with_context(
        &self,
        token: &CancellationToken,
        id: Id,
        ctx: &BlockContext,
    ) -> Result<()>;
}
