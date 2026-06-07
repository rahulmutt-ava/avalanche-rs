// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The VM [`Factory`] / [`VmManager`] registry (`vms.Factory` / `vms.Manager`,
//! specs 07 §8.1) plus [`ChainParameters`] (`chains.ChainParameters`).
//!
//! The manager registers VM factories by VM id, tracks the version reported by
//! a freshly created VM (probed once at registration), and embeds the
//! [`Aliaser`] so a VM id is always aliased to (at least) its own string form.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;

use crate::aliaser::Aliaser;
use crate::error::{Error, Result};

/// `vms.Factory` — creates VM instances. The boxed `Any` is downcast to a
/// concrete `ChainVm` (or legacy DAG VM) by the chain-creation pipeline.
#[async_trait]
pub trait Factory: Send + Sync {
    /// `New(log)` — builds a fresh VM instance.
    ///
    /// # Errors
    /// Returns an [`Error`] if the VM cannot be constructed.
    async fn new_vm(&self) -> Result<Box<dyn Any + Send>>;
}

/// The subset of the VM surface the manager probes at registration: it creates
/// a VM, asks its `Version` (for the versions map / logging), then `Shutdown`s
/// it (Go `manager.RegisterFactory`). A concrete VM exposes this by
/// implementing [`ProbeableVm`]; the manager downcasts the factory's product.
#[async_trait]
pub trait ProbeableVm: Send + Sync {
    /// `Version` — the VM's semantic version string.
    ///
    /// # Errors
    /// Returns an [`Error`] if the version cannot be read.
    async fn version(&self, token: &CancellationToken) -> Result<String>;

    /// `Shutdown` — release the probed VM's resources.
    ///
    /// # Errors
    /// Returns an [`Error`] if shutdown fails.
    async fn shutdown(&mut self, token: &CancellationToken) -> Result<()>;
}

/// `chains.ChainParameters` — the inputs to create one chain.
#[derive(Clone, Debug)]
pub struct ChainParameters {
    /// The unique id of this chain.
    pub id: Id,
    /// The subnet this chain is a part of.
    pub subnet_id: Id,
    /// The genesis data of this chain's ledger.
    pub genesis_data: Vec<u8>,
    /// The id of the VM this chain runs.
    pub vm_id: Id,
    /// The ids of the feature extensions this chain runs.
    pub fx_ids: Vec<Id>,
    /// Custom beacons for this chain (overrides the subnet's beacons if set).
    pub custom_beacons: Vec<Id>,
}

/// `vms.Manager` — the factory registry keyed by VM id with an embedded
/// [`Aliaser`] and the per-VM version map.
pub struct VmManager {
    inner: RwLock<Inner>,
    aliaser: Arc<Aliaser>,
}

struct Inner {
    /// `vmID -> factory`.
    factories: HashMap<Id, Arc<dyn Factory>>,
    /// `vmID -> reported version` (probed once at registration).
    versions: HashMap<Id, String>,
    /// Registration order, so `list_factories` is deterministic.
    order: Vec<Id>,
}

impl Default for VmManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VmManager {
    /// Builds an empty manager with a fresh [`Aliaser`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(Inner {
                factories: HashMap::new(),
                versions: HashMap::new(),
                order: Vec::new(),
            }),
            aliaser: Arc::new(Aliaser::new()),
        }
    }

    /// The embedded aliaser (used by the chain pipeline for `primary_alias`).
    #[must_use]
    pub fn aliaser(&self) -> Arc<Aliaser> {
        Arc::clone(&self.aliaser)
    }

    /// `GetFactory(vmID)` — the registered factory for `vm_id`.
    ///
    /// # Errors
    /// [`Error::NotFound`] if no factory is registered under `vm_id`.
    pub fn get_factory(&self, vm_id: Id) -> Result<Arc<dyn Factory>> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner
            .factories
            .get(&vm_id)
            .cloned()
            .ok_or(Error::NotFound)
    }

    /// `RegisterFactory(vmID, factory)` — registers a factory, probing the
    /// created VM's `Version` (recorded under the VM id's primary alias) then
    /// `Shutdown`. A VM id may be registered only once.
    ///
    /// The VM id is aliased to its own string form so it is always resolvable.
    ///
    /// # Errors
    /// [`Error::VmAlreadyRegistered`] if `vm_id` already has a factory;
    /// propagates VM `version`/`shutdown` errors and factory-construction errors.
    pub async fn register_factory(
        &self,
        token: &CancellationToken,
        vm_id: Id,
        factory: Arc<dyn Factory>,
    ) -> Result<()> {
        // Reject duplicates before doing any work (Go errDuplicatedVM).
        {
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            if inner.factories.contains_key(&vm_id) {
                return Err(Error::VmAlreadyRegistered);
            }
        }

        // Probe the created VM's version, then shut it down (Go: log the version
        // of every registered VM at startup).
        let version = self.probe_version(token, factory.as_ref()).await?;

        // Alias the VM id to its own string form (idempotent: ignore a collision
        // if it was already aliased).
        let _ = self.aliaser.alias(vm_id, &vm_id.to_string());

        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        // Re-check under the write lock (another registration may have raced).
        if inner.factories.contains_key(&vm_id) {
            return Err(Error::VmAlreadyRegistered);
        }
        inner.factories.insert(vm_id, factory);
        inner.versions.insert(vm_id, version);
        inner.order.push(vm_id);
        Ok(())
    }

    /// Creates a VM from `factory`, probes `Version`, then `Shutdown`s it. If the
    /// created VM does not implement [`ProbeableVm`], the version is unknown and
    /// the probe is skipped (Go logs `"unknown"`).
    async fn probe_version(
        &self,
        token: &CancellationToken,
        factory: &dyn Factory,
    ) -> Result<String> {
        let mut vm = factory.new_vm().await?;
        // A factory whose product is probeable boxes it as a [`DynProbe`] so the
        // manager can call `Version`/`Shutdown` without knowing the concrete VM
        // type (the Rust analogue of Go's `vm.(common.VM)` type assertion).
        // Otherwise the version is unknown and the probe is skipped (Go logs
        // `"unknown"`).
        if let Some(probe) = vm.downcast_mut::<DynProbe>() {
            let version = probe.0.version(token).await?;
            probe.0.shutdown(token).await?;
            return Ok(version);
        }
        Ok("unknown".to_string())
    }

    /// `ListFactories()` — registered VM ids in registration order.
    #[must_use]
    pub fn list_factories(&self) -> Vec<Id> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.order.clone()
    }

    /// `Versions()` — `primaryAlias -> version` for every registered VM.
    #[must_use]
    pub fn versions(&self) -> HashMap<String, String> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner
            .versions
            .iter()
            .map(|(id, v)| (self.aliaser.primary_alias_or_default(*id), v.clone()))
            .collect()
    }
}

/// A type-erased [`ProbeableVm`] a factory boxes as its product (inside the
/// `Box<dyn Any + Send>`) so the manager can probe it without knowing the
/// concrete VM type.
pub struct DynProbe(pub Box<dyn ProbeableVm>);
