// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The state-sync skeleton (port of `snow/engine/snowman/syncer/`, specs 06
//! §4.4).
//!
//! Before bootstrapping, a node *may* fetch a recent state summary
//! (`GetStateSummaryFrontier` → `GetAcceptedStateSummary` → hand the chosen
//! summary to the VM's state syncer). Engines / VMs that do not support state
//! sync use the **no-op** state-summary handlers — they log and drop the
//! state-summary ops. This module provides that no-op skeleton plus a probe of
//! the VM's [`StateSyncableVm`] capability; the full out-of-band summary fetch
//! lands in a later milestone.
//!
//! ## Port note
//!
//! Go's `syncer.stateSyncer` runs the full frontier → accepted-summary →
//! VM-sync flow with weight thresholds. Here we implement the capability probe
//! (`state_sync_enabled`) and the no-op inbound handlers required by spec 06
//! §4.4 for engines/VMs that disable state sync; the active syncing flow is
//! deferred. See `tests/PORTING.md`.

use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::block::ChainVm;

use crate::error::Result;

/// The state-sync engine skeleton with no-op state-summary handlers.
pub struct StateSyncer<V> {
    vm: Arc<Mutex<V>>,
    token: CancellationToken,
}

impl<V> StateSyncer<V>
where
    V: ChainVm,
{
    /// Builds a state syncer over the supplied VM.
    pub fn new(vm: Arc<Mutex<V>>, token: CancellationToken) -> Self {
        Self { vm, token }
    }

    /// Whether the VM supports (and has enabled) state sync. A VM that does not
    /// implement [`StateSyncableVm`](ava_vm::block::StateSyncableVm) reports
    /// `false`.
    ///
    /// # Errors
    /// Propagates a fatal VM error from `state_sync_enabled`.
    pub async fn enabled(&self) -> Result<bool> {
        let vm = self.vm.lock().await;
        match vm.as_state_syncable() {
            Some(ss) => Ok(ss.state_sync_enabled(&self.token).await?),
            None => Ok(false),
        }
    }

    // ---- no-op inbound state-summary handlers (spec 06 §4.4) ----

    /// `StateSummaryFrontier` — drop (no-op when state sync is disabled).
    ///
    /// # Errors
    /// Never errors.
    pub async fn state_summary_frontier(
        &mut self,
        _node: NodeId,
        _req: u32,
        _summary: &[u8],
    ) -> Result<()> {
        Ok(())
    }

    /// `GetStateSummaryFrontierFailed` — drop.
    ///
    /// # Errors
    /// Never errors.
    pub async fn get_state_summary_frontier_failed(
        &mut self,
        _node: NodeId,
        _req: u32,
    ) -> Result<()> {
        Ok(())
    }

    /// `AcceptedStateSummary` — drop.
    ///
    /// # Errors
    /// Never errors.
    pub async fn accepted_state_summary(
        &mut self,
        _node: NodeId,
        _req: u32,
        _summary_ids: &[Id],
    ) -> Result<()> {
        Ok(())
    }

    /// `GetAcceptedStateSummaryFailed` — drop.
    ///
    /// # Errors
    /// Never errors.
    pub async fn get_accepted_state_summary_failed(
        &mut self,
        _node: NodeId,
        _req: u32,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ava_vm::testutil::init_test_vm;

    /// A VM that does not implement `StateSyncableVm` reports state sync
    /// disabled, and the no-op handlers drop cleanly.
    #[tokio::test]
    async fn state_sync_disabled_and_handlers_noop() {
        let token = CancellationToken::new();
        let vm = init_test_vm(&token).await.expect("vm");
        let mut syncer = StateSyncer::new(Arc::new(Mutex::new(vm)), token);

        assert!(!syncer.enabled().await.expect("enabled probe"));

        let node = NodeId::from([1u8; 20]);
        syncer
            .state_summary_frontier(node, 1, &[1, 2, 3])
            .await
            .expect("frontier noop");
        syncer
            .get_state_summary_frontier_failed(node, 1)
            .await
            .expect("frontier failed noop");
        syncer
            .accepted_state_summary(node, 2, &[Id::EMPTY])
            .await
            .expect("accepted noop");
        syncer
            .get_accepted_state_summary_failed(node, 2)
            .await
            .expect("accepted failed noop");
    }
}
