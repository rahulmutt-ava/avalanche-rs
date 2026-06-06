// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `block.StateSyncableVM` / `block.StateSummary` / `block.StateSyncMode`
//! (specs 07 §2.5; Go `snow/engine/snowman/block/state_syncable_vm.go` +
//! `state_summary.go`).

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::error::Result;

/// `block.StateSyncMode` — the mode the VM selected when accepting a summary.
///
/// Discriminants match Go's `iota + 1`: `StateSyncSkipped == 1`,
/// `StateSyncStatic == 2`, `StateSyncDynamic == 3`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[repr(u32)]
pub enum StateSyncMode {
    /// `StateSyncSkipped` — the VM declined to state-sync this summary.
    Skipped = 1,
    /// `StateSyncStatic` — the VM will sync before resuming consensus.
    Static = 2,
    /// `StateSyncDynamic` — the VM can sync while consensus continues.
    Dynamic = 3,
}

/// `block.StateSummary` — a summary of the VM state at a given height.
#[async_trait]
pub trait StateSummary: Send + Sync {
    /// The summary's id.
    fn id(&self) -> Id;

    /// The height the summary describes.
    fn height(&self) -> u64;

    /// The canonical serialized bytes of the summary.
    fn bytes(&self) -> &[u8];

    /// `Accept` — instruct the VM to sync to this summary, returning the
    /// [`StateSyncMode`] it adopted.
    async fn accept(&self, token: &CancellationToken) -> Result<StateSyncMode>;
}

/// `block.StateSyncableVM` — the optional state-sync capability.
#[async_trait]
pub trait StateSyncableVm: Send + Sync {
    /// `StateSyncEnabled` — whether state sync is enabled for this VM.
    async fn state_sync_enabled(&self, token: &CancellationToken) -> Result<bool>;

    /// `GetOngoingSyncStateSummary` — the summary of an in-progress sync.
    /// `Err(Error::NotFound)` if none is ongoing.
    async fn get_ongoing_sync_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>>;

    /// `GetLastStateSummary` — the most recent available summary.
    async fn get_last_state_summary(
        &self,
        token: &CancellationToken,
    ) -> Result<Arc<dyn StateSummary>>;

    /// `ParseStateSummary` — parse a summary from its bytes.
    async fn parse_state_summary(
        &self,
        token: &CancellationToken,
        bytes: &[u8],
    ) -> Result<Arc<dyn StateSummary>>;

    /// `GetStateSummary` — the summary at the given height.
    /// `Err(Error::NotFound)` if unknown.
    async fn get_state_summary(
        &self,
        token: &CancellationToken,
        height: u64,
    ) -> Result<Arc<dyn StateSummary>>;
}
