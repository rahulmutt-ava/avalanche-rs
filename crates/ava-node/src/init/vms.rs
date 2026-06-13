// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init steps 4, 19 and 21 (specs/12 §2.2): the VM aliaser/manager, the
//! default VM aliases (mirror Go `genesis.VMAliases`), and the VM registry +
//! plugin runtime manager (mirror Go `initVMs`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_chains::manager::{Factory, VmManager};
use ava_chains::registry::{VmGetter, VmRegistry};
use ava_types::id::Id;

use crate::error::Result;
use crate::init::metrics::NodeMetrics;

/// Step 4: `VMAliaser` + `VMManager` (the Rust [`VmManager`] embeds its
/// aliaser), seeding the configured `--vm-aliases` (mirror Go `node.New`).
///
/// # Errors
/// Propagates alias conflicts.
pub fn init_vm_manager(vm_aliases: &HashMap<Id, Vec<String>>) -> Result<Arc<VmManager>> {
    let manager = Arc::new(VmManager::new());
    let aliaser = manager.aliaser();
    for (vm_id, aliases) in vm_aliases {
        for alias in aliases {
            aliaser.alias(*vm_id, alias)?;
        }
    }
    Ok(manager)
}

/// Step 19: the default VM aliases (mirror Go `genesis.VMAliases`):
/// `platformvm → platform`, `avm → avm`, `evm → evm`, plus the three fx ids.
///
/// # Errors
/// Propagates alias conflicts (a user `--vm-aliases` entry that collides).
pub fn add_default_vm_aliases(manager: &VmManager) -> Result<()> {
    tracing::info!("adding the default VM aliases");
    let aliaser = manager.aliaser();
    let defaults: [([u8; 32], &str); 6] = [
        (ava_genesis::chains::PLATFORM_VM_ID_BYTES, "platform"),
        (ava_genesis::chains::AVM_ID_BYTES, "avm"),
        (ava_genesis::chains::EVM_ID_BYTES, "evm"),
        (ava_genesis::chains::SECP256K1FX_ID_BYTES, "secp256k1fx"),
        (ava_genesis::chains::NFTFX_ID_BYTES, "nftfx"),
        (ava_genesis::chains::PROPERTYFX_ID_BYTES, "propertyfx"),
    ];
    for (vm_id, alias) in defaults {
        aliaser.alias(Id::from(vm_id), alias)?;
    }
    Ok(())
}

/// The slice of the rpcchainvm plugin host the node consumes (Go
/// `runtime.Manager`: tracked plugin subprocesses, stopped at shutdown step
/// 12). **Narrow seam (M8.29, `tests/PORTING.md`):** subprocess tracking lands
/// with the `ava-vm-rpc` host wiring; [`NoopRuntimeManager`] tracks nothing.
pub trait RuntimeManager: Send + Sync {
    /// Kill every tracked plugin subprocess (Go `runtime.Manager.Stop`).
    fn stop(&self);
}

/// The deferral implementation of [`RuntimeManager`].
pub struct NoopRuntimeManager;

impl RuntimeManager for NoopRuntimeManager {
    fn stop(&self) {}
}

/// A [`VmGetter`] that discovers no plugin VMs. **Narrow seam (M8.29,
/// `tests/PORTING.md`):** the `plugin-dir` scanner (filename → VM id, probe
/// via the rpcchainvm handshake) lands with the plugin-host milestone.
struct EmptyVmGetter;

#[async_trait]
impl VmGetter for EmptyVmGetter {
    async fn get(
        &self,
        _token: &CancellationToken,
    ) -> ava_chains::Result<Vec<(Id, Arc<dyn Factory>)>> {
        Ok(Vec::new())
    }
}

/// Everything step 21 hands back to `Node::new`.
pub struct VmsInit {
    /// The VM registry (plugin reload surface for the admin `loadVMs`).
    pub registry: Arc<VmRegistry>,
    /// The plugin runtime manager seam.
    pub runtime_manager: Arc<dyn RuntimeManager>,
}

/// Step 21: the VM registry + runtime manager (mirror Go `initVMs`).
///
/// Go registers the built-in platformvm/avm/coreth factories here; the Rust
/// `ava_chains::Factory` impls for P/X/C do not exist yet, so registration is
/// a documented deferral (`tests/PORTING.md`) — the registry starts empty and
/// `reload` discovers nothing until the plugin scanner lands.
///
/// # Errors
/// Metrics-namespace registration failures.
pub async fn init_vms(
    metrics: &NodeMetrics,
    vm_manager: &Arc<VmManager>,
    token: &CancellationToken,
) -> Result<VmsInit> {
    tracing::info!("initializing VMs");

    let runtime_manager: Arc<dyn RuntimeManager> = Arc::new(NoopRuntimeManager);

    // Go registers a per-chain label gatherer under `avalanche_rpcchainvm`.
    let rpcchainvm_gatherer = Arc::new(ava_api::metrics::LabelGatherer::new(
        ava_api::metrics::CHAIN_LABEL,
    ));
    {
        use ava_api::metrics::{Gatherer, MultiGatherer};
        metrics.gatherer.register(
            &crate::init::namespace::rpcchainvm(),
            Arc::clone(&rpcchainvm_gatherer) as Arc<dyn Gatherer>,
        )?;
    }

    let registry = Arc::new(VmRegistry::new(
        Box::new(EmptyVmGetter),
        Arc::clone(vm_manager),
    ));

    // Mirror Go: reload once at init, logging per-VM failures.
    match registry.reload(token).await {
        Ok((_new_vms, failed)) => {
            for (vm_id, err) in failed {
                tracing::error!(%vm_id, error = %err, "failed to register VM");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to reload VM registry");
        }
    }

    Ok(VmsInit {
        registry,
        runtime_manager,
    })
}
