// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/registry.VMRegistry` (specs 07 §8.1) — installs plugin VMs discovered on
//! disk into the [`VmManager`].
//!
//! The [`VmGetter`] scans the plugin directory and yields `(vmID, factory)`
//! pairs for every plugin binary (building an `rpcchainvm` host factory per
//! binary — the concrete `VmGetter` lands with the rpcchainvm plugin host,
//! specs 12). [`VmRegistry::reload`] registers all not-yet-installed VMs,
//! returning `(installed, failed)`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::manager::{Factory, VmManager};

/// `vms/registry.VMGetter` — discovers installable VMs (plugin binaries on
/// disk). Returns the full set of `(vmID, factory)` pairs available; the
/// registry diffs this against what is already registered.
#[async_trait]
pub trait VmGetter: Send + Sync {
    /// `Get()` — every discoverable VM `(id, factory)` pair.
    ///
    /// # Errors
    /// Returns an [`Error`] if the plugin directory cannot be scanned.
    async fn get(&self, token: &CancellationToken) -> Result<Vec<(Id, Arc<dyn Factory>)>>;
}

/// `vms/registry.VMRegistry` — installs plugin VMs into the manager.
pub struct VmRegistry {
    getter: Box<dyn VmGetter>,
    manager: Arc<VmManager>,
}

impl VmRegistry {
    /// Builds a registry over a `getter` (plugin discovery) and the shared
    /// `manager`.
    #[must_use]
    pub fn new(getter: Box<dyn VmGetter>, manager: Arc<VmManager>) -> Self {
        Self { getter, manager }
    }

    /// `Reload()` — register every not-yet-installed discovered VM. Returns the
    /// ids that were freshly `installed` and a map of ids that `failed` to
    /// register (keyed to the error). Already-registered VMs are skipped.
    ///
    /// # Errors
    /// Propagates a discovery (`getter`) error; per-VM registration failures are
    /// collected into the returned `failed` map rather than aborting.
    pub async fn reload(&self, token: &CancellationToken) -> Result<(Vec<Id>, HashMap<Id, Error>)> {
        let discovered = self.getter.get(token).await?;

        let mut installed = Vec::new();
        let mut failed = HashMap::new();
        for (vm_id, factory) in discovered {
            // Skip VMs already registered (Go: only register the diff).
            if self.manager.get_factory(vm_id).is_ok() {
                continue;
            }
            match self.manager.register_factory(token, vm_id, factory).await {
                Ok(()) => installed.push(vm_id),
                Err(e) => {
                    failed.insert(vm_id, e);
                }
            }
        }
        Ok((installed, failed))
    }
}
