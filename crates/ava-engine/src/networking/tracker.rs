// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`ResourceTracker`] + [`Targeter`] (port of `snow/networking/tracker/`, specs
//! 06 §5.6).
//!
//! The tracker measures per-peer CPU and disk usage attributed to processing
//! their messages; the [`Targeter`] computes each peer's fair-share budget (a
//! base at-large allocation plus a stake-weighted validator bonus). The handler
//! uses these to prioritize and throttle the async message pool.
//!
//! Float math, off the consensus-determinism path (06 §5.6).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_validators::ValidatorManager;

/// Measures per-peer resource usage attributed to message processing.
pub trait ResourceTracker: Send + Sync {
    /// Record `usage` (e.g. CPU-seconds, disk bytes) attributed to `node`.
    fn record_usage(&self, node: NodeId, usage: f64);

    /// The current usage attributed to `node`.
    fn usage(&self, node: NodeId) -> f64;

    /// The total usage across all peers.
    fn total_usage(&self) -> f64;
}

/// An in-memory accumulating [`ResourceTracker`].
#[derive(Default)]
pub struct CumulativeTracker {
    usage: Mutex<HashMap<NodeId, f64>>,
}

impl CumulativeTracker {
    /// Create an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ResourceTracker for CumulativeTracker {
    fn record_usage(&self, node: NodeId, usage: f64) {
        if let Ok(mut map) = self.usage.lock() {
            *map.entry(node).or_insert(0.0) += usage;
        }
    }

    fn usage(&self, node: NodeId) -> f64 {
        self.usage
            .lock()
            .ok()
            .and_then(|m| m.get(&node).copied())
            .unwrap_or(0.0)
    }

    fn total_usage(&self) -> f64 {
        self.usage
            .lock()
            .map(|m| m.values().sum())
            .unwrap_or(0.0)
    }
}

/// Fair-share budget config (Go `tracker.TargeterConfig`).
#[derive(Clone, Debug)]
pub struct TargeterConfig {
    /// The amount of the resource to split over validators, weighted by stake.
    pub vdr_alloc: f64,
    /// The total non-validator usage above which allocations are stake-only.
    pub max_non_vdr_usage: f64,
    /// The per-node non-validator at-large allocation cap.
    pub max_non_vdr_node_usage: f64,
}

/// Computes each peer's fair-share resource budget.
pub struct Targeter {
    config: TargeterConfig,
    vdrs: Arc<dyn ValidatorManager>,
    tracker: Arc<dyn ResourceTracker>,
    subnet: Id,
}

impl Targeter {
    /// Build a targeter over the primary-network validator set + tracker.
    #[must_use]
    pub fn new(
        config: TargeterConfig,
        vdrs: Arc<dyn ValidatorManager>,
        tracker: Arc<dyn ResourceTracker>,
        subnet: Id,
    ) -> Self {
        Self {
            config,
            vdrs,
            tracker,
            subnet,
        }
    }

    /// The target usage budget for `node` (Go `targeter.TargetUsage`):
    /// `min(remaining at-large, per-node at-large cap)` plus the node's
    /// stake-weighted slice of the validator allocation.
    #[must_use]
    pub fn target_usage(&self, node: NodeId) -> f64 {
        let usage = self.tracker.total_usage();
        let base_alloc = (self.config.max_non_vdr_usage - usage)
            .max(0.0)
            .min(self.config.max_non_vdr_node_usage);

        let weight = self.vdrs.get_weight(self.subnet, node);
        if weight == 0 {
            return base_alloc;
        }
        let Ok(total_weight) = self.vdrs.total_weight(self.subnet) else {
            return base_alloc;
        };
        if total_weight == 0 {
            return base_alloc;
        }
        // weight/total ratio in [0,1]; precision loss is acceptable (off the
        // consensus path).
        #[allow(clippy::cast_precision_loss)]
        let ratio = weight as f64 / total_weight as f64;
        self.config.vdr_alloc * ratio + base_alloc
    }
}

#[cfg(test)]
mod tests {
    use ava_validators::DefaultManager;

    use super::*;

    #[test]
    fn tracker_accumulates_per_peer() {
        let t = CumulativeTracker::new();
        let a = NodeId::from([1u8; 20]);
        let b = NodeId::from([2u8; 20]);
        t.record_usage(a, 1.5);
        t.record_usage(a, 0.5);
        t.record_usage(b, 3.0);
        assert!((t.usage(a) - 2.0).abs() < f64::EPSILON);
        assert!((t.usage(b) - 3.0).abs() < f64::EPSILON);
        assert!((t.total_usage() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn targeter_gives_validators_a_stake_weighted_bonus() {
        let subnet = Id::from([0u8; 32]);
        let vdr = NodeId::from([1u8; 20]);
        let non_vdr = NodeId::from([2u8; 20]);

        let mgr = Arc::new(DefaultManager::new());
        mgr.add_weight(subnet, vdr, 100).unwrap();

        let tracker: Arc<dyn ResourceTracker> = Arc::new(CumulativeTracker::new());
        let cfg = TargeterConfig {
            vdr_alloc: 10.0,
            max_non_vdr_usage: 4.0,
            max_non_vdr_node_usage: 1.0,
        };
        let targeter = Targeter::new(cfg, mgr, tracker, subnet);

        // Non-validator gets only the base at-large allocation (capped at 1.0).
        let non = targeter.target_usage(non_vdr);
        assert!((non - 1.0).abs() < f64::EPSILON);

        // Validator gets the full vdr_alloc (sole staker) plus the base.
        let val = targeter.target_usage(vdr);
        assert!((val - 11.0).abs() < f64::EPSILON, "got {val}");
    }
}
