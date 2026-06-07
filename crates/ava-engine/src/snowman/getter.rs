// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `AllGetsServer` — the read-only request side every engine serves (port of
//! `snow/engine/snowman/getter/getter.go`, specs 06 §4.3).
//!
//! `Get`/`GetAncestors`/`GetAcceptedFrontier`/`GetAccepted` are answered from the
//! local VM regardless of engine phase (bootstrapping or normal op). State-summary
//! requests are answered only if the VM is a [`StateSyncableVm`]; otherwise they
//! are logged and dropped (the no-op path of specs 06 §4.4).

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_vm::block::{ChainVm, get_ancestors};
use ava_vm::error::Error as VmError;

use crate::common::sender::Sender;
use crate::error::Result;

/// `constants.MaxContainersLen` — `4 * DefaultMaxMessageSize / 5`, with
/// `DefaultMaxMessageSize = 2 MiB`.
pub const MAX_CONTAINERS_LEN: usize = 4 * 2 * 1024 * 1024 / 5;

/// Default `maxContainersGetAncestors` (Go `chains/manager.go`).
pub const DEFAULT_MAX_CONTAINERS_GET_ANCESTORS: usize = 2000;

/// Default `bootstrap-max-time-get-ancestors` (Go config default).
pub const DEFAULT_MAX_TIME_GET_ANCESTORS: Duration = Duration::from_millis(50);

/// The read-only `Get*` server. Holds an `Arc` to the VM and the engine's
/// [`Sender`] so it can reply directly.
pub struct Getter<V, S> {
    vm: Arc<tokio::sync::Mutex<V>>,
    sender: Arc<S>,
    max_containers_get_ancestors: usize,
    max_time_get_ancestors: Duration,
    token: CancellationToken,
}

impl<V, S> Getter<V, S>
where
    V: ChainVm,
    S: Sender,
{
    /// Builds a getter with the default ancestor-fetch limits.
    pub fn new(vm: Arc<tokio::sync::Mutex<V>>, sender: Arc<S>, token: CancellationToken) -> Self {
        Self {
            vm,
            sender,
            max_containers_get_ancestors: DEFAULT_MAX_CONTAINERS_GET_ANCESTORS,
            max_time_get_ancestors: DEFAULT_MAX_TIME_GET_ANCESTORS,
            token,
        }
    }

    /// `GetAcceptedFrontier` — reply with the VM's last-accepted block id.
    ///
    /// # Errors
    /// Propagates a fatal VM error from `LastAccepted`.
    pub async fn get_accepted_frontier(&self, node: NodeId, req: u32) -> Result<()> {
        let last_accepted = {
            let vm = self.vm.lock().await;
            vm.last_accepted(&self.token).await?
        };
        self.sender.send_accepted_frontier(node, req, last_accepted);
        Ok(())
    }

    /// `GetAccepted` — reply with the subset of `container_ids` whose blocks are
    /// accepted at or below the last-accepted height (Go `getter.GetAccepted`).
    ///
    /// # Errors
    /// Propagates a fatal VM error from `LastAccepted`/`GetBlock` of the tip.
    pub async fn get_accepted(&self, node: NodeId, req: u32, container_ids: &[Id]) -> Result<()> {
        let mut accepted = Vec::new();
        {
            let vm = self.vm.lock().await;
            let last_accepted_id = vm.last_accepted(&self.token).await?;
            let last_accepted = vm.get_block(&self.token, last_accepted_id).await?;
            let last_height = last_accepted.height();

            for &blk_id in container_ids {
                let blk = match vm.get_block(&self.token, blk_id).await {
                    Ok(blk) => blk,
                    Err(_) => continue,
                };
                let height = blk.height();
                if height > last_height {
                    continue;
                }
                match vm.get_block_id_at_height(&self.token, height).await {
                    Ok(accepted_id) if accepted_id == blk_id => accepted.push(blk_id),
                    _ => {}
                }
            }
        }
        self.sender.send_accepted(node, req, &accepted);
        Ok(())
    }

    /// `Get` — reply with a `Put` of the requested block, or drop if unknown.
    ///
    /// # Errors
    /// Propagates a fatal VM error (a `NotFound` is dropped, not fatal).
    pub async fn get(&self, node: NodeId, req: u32, container_id: Id) -> Result<()> {
        let blk = {
            let vm = self.vm.lock().await;
            vm.get_block(&self.token, container_id).await
        };
        match blk {
            Ok(blk) => {
                self.sender.send_put(node, req, blk.bytes().to_vec());
                Ok(())
            }
            // The block is unknown / pruned / the peer is misbehaving: drop.
            Err(VmError::NotFound) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// `GetAncestors` — reply with the requested block plus a best-effort chain
    /// of ancestors, bounded by the container/byte/time budget.
    ///
    /// # Errors
    /// A fetch failure is dropped (Go logs + returns nil); never fatal here.
    pub async fn get_ancestors(&self, node: NodeId, req: u32, container_id: Id) -> Result<()> {
        let ancestors = {
            let vm = self.vm.lock().await;
            get_ancestors(
                &*vm,
                &self.token,
                container_id,
                self.max_containers_get_ancestors,
                MAX_CONTAINERS_LEN,
                self.max_time_get_ancestors,
            )
            .await
        };
        match ancestors {
            Ok(containers) => {
                self.sender.send_ancestors(node, req, containers);
                Ok(())
            }
            // Couldn't get ancestors: drop the request (Go behavior).
            Err(_) => Ok(()),
        }
    }

    /// `GetStateSummaryFrontier` — reply with the VM's last state summary if the
    /// VM is state-syncable; otherwise drop.
    ///
    /// # Errors
    /// A summary-fetch failure is dropped (Go behavior); never fatal.
    pub async fn get_state_summary_frontier(&self, node: NodeId, req: u32) -> Result<()> {
        let summary = {
            let vm = self.vm.lock().await;
            let Some(ss) = vm.as_state_syncable() else {
                return Ok(());
            };
            ss.get_last_state_summary(&self.token).await
        };
        if let Ok(summary) = summary {
            self.sender
                .send_state_summary_frontier(node, req, summary.bytes().to_vec());
        }
        Ok(())
    }

    /// `GetAcceptedStateSummary` — reply with the summary ids for the requested
    /// heights (empty if none requested / VM not state-syncable).
    ///
    /// # Errors
    /// Per-height failures are skipped; never fatal here.
    pub async fn get_accepted_state_summary(
        &self,
        node: NodeId,
        req: u32,
        heights: &[u64],
    ) -> Result<()> {
        if heights.is_empty() {
            self.sender.send_accepted_state_summary(node, req, &[]);
            return Ok(());
        }
        let mut summary_ids = Vec::new();
        {
            let vm = self.vm.lock().await;
            let Some(ss) = vm.as_state_syncable() else {
                return Ok(());
            };
            for &height in heights {
                if let Ok(summary) = ss.get_state_summary(&self.token, height).await {
                    summary_ids.push(summary.id());
                }
            }
        }
        self.sender
            .send_accepted_state_summary(node, req, &summary_ids);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limit constants match the Go defaults.
    #[test]
    fn limit_constants() {
        assert_eq!(MAX_CONTAINERS_LEN, 1_677_721);
        assert_eq!(DEFAULT_MAX_CONTAINERS_GET_ANCESTORS, 2000);
        assert_eq!(DEFAULT_MAX_TIME_GET_ANCESTORS, Duration::from_millis(50));
    }
}
