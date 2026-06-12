// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Init step 15 (specs/12 §2.2): the system resource manager + CPU/disk
//! trackers and targeters (mirror Go `initResourceManager` /
//! `initCPUTargeter` / `initDiskTargeter`).

use std::sync::Arc;

use ava_config::node::Config;
use ava_engine::networking::tracker::{CumulativeTracker, ResourceTracker, Targeter};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_validators::ValidatorManager;

use crate::error::Result;
use crate::init::metrics::NodeMetrics;

/// The slice of Go `resource.Manager` the node consumes (process CPU/disk
/// polling + available disk space). **Narrow seam (M8.29,
/// `tests/PORTING.md`):** the Rust workspace has no system-resource poller
/// yet; [`NoopSystemResources`] reports unbounded disk space so the
/// disk-space health check stays green until the real poller lands.
pub trait SystemResourceManager: Send + Sync {
    /// Begin tracking the process (Go `TrackProcess(os.Getpid())`).
    fn track_process(&self, pid: u32);
    /// Bytes available on the database volume (Go
    /// `DiskTracker().AvailableDiskBytes()`).
    fn available_disk_bytes(&self) -> u64;
    /// Percentage (0–100) available on the database volume.
    fn available_disk_percentage(&self) -> u64;
    /// Stop the poller (shutdown step 3, M8.30).
    fn shutdown(&self);
}

/// The deferral implementation of [`SystemResourceManager`].
pub struct NoopSystemResources;

impl SystemResourceManager for NoopSystemResources {
    fn track_process(&self, _pid: u32) {}
    fn available_disk_bytes(&self) -> u64 {
        u64::MAX
    }
    fn available_disk_percentage(&self) -> u64 {
        100
    }
    fn shutdown(&self) {}
}

/// The node's resource handles (Go `n.resourceManager` / `n.resourceTracker` /
/// `n.cpuTargeter` / `n.diskTargeter`).
pub struct Resources {
    /// The system poller seam.
    pub manager: Arc<dyn SystemResourceManager>,
    /// CPU usage tracker (consumed by inbound throttling / targeters).
    pub cpu_tracker: Arc<dyn ResourceTracker>,
    /// Disk usage tracker.
    pub disk_tracker: Arc<dyn ResourceTracker>,
    /// Per-node CPU allocation targeter.
    pub cpu_targeter: Arc<Targeter>,
    /// Per-node disk allocation targeter.
    pub disk_targeter: Arc<Targeter>,
}

/// Step 15: build the resource manager seam, the CPU/disk trackers, and both
/// targeters (Go runs these as three consecutive init calls; the spec folds
/// them into one step).
///
/// # Errors
/// Metrics-namespace registration failures.
pub fn init_resource_manager(
    config: &Config,
    metrics: &NodeMetrics,
    validators: &Arc<dyn ValidatorManager>,
) -> Result<Resources> {
    let _system_resources_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::system_resources(),
    )?;
    let _resource_tracker_registry = ava_api::metrics::make_and_register(
        metrics.gatherer.as_ref(),
        &crate::init::namespace::resource_tracker(),
    )?;

    let manager: Arc<dyn SystemResourceManager> = Arc::new(NoopSystemResources);
    manager.track_process(std::process::id());

    let cpu_tracker: Arc<dyn ResourceTracker> = Arc::new(CumulativeTracker::new());
    let disk_tracker: Arc<dyn ResourceTracker> = Arc::new(CumulativeTracker::new());

    let cpu_targeter = Arc::new(Targeter::new(
        config.cpu_targeter_config.clone(),
        Arc::clone(validators),
        Arc::clone(&cpu_tracker),
        PRIMARY_NETWORK_ID,
    ));
    let disk_targeter = Arc::new(Targeter::new(
        config.disk_targeter_config.clone(),
        Arc::clone(validators),
        Arc::clone(&disk_tracker),
        PRIMARY_NETWORK_ID,
    ));

    Ok(Resources {
        manager,
        cpu_tracker,
        disk_tracker,
        cpu_targeter,
        disk_targeter,
    })
}
