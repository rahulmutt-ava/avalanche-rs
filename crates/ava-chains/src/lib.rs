// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-chains` — the chain manager (port of `chains/manager.go` +
//! `chains/atomic`, specs 07 §8 / §3.1).
//!
//! Tier T3 (node services). This crate wires the consensus stack into running
//! chains. It owns:
//!
//! * [`manager`] — the VM [`Factory`] / [`VmManager`] registry (`vms.Manager`),
//!   the per-VM version map, and [`ChainParameters`] (§8.1).
//! * [`registry`] — [`VmRegistry`] + [`VmGetter`], installing plugin VMs (§8.1).
//! * [`aliaser`] — the bidirectional [`Aliaser`] + [`AliaserReader`] (`bc_lookup`,
//!   §8.3).
//! * [`subnet`] — the [`Subnet`] consensus parameters + allowed-node ACL (§8.3).
//! * [`atomic`] — the cross-chain atomic [`Memory`](atomic::Memory) backing the
//!   `SharedMemory` views (§3.1).
//! * [`create_chain`] — the `create_snowman_chain` pipeline reproducing the exact
//!   VM wrapping order + DB stack + engine/handler/router wiring (§8.2, M3.27).

#![forbid(unsafe_code)]

pub mod aliaser;
pub mod atomic;
pub mod create_chain;
pub mod error;
pub mod manager;
pub mod registry;
pub mod subnet;

pub use aliaser::{Aliaser, AliaserReader};
pub use error::{Error, Result};
pub use manager::{ChainParameters, DynProbe, Factory, ProbeableVm, VmManager};
pub use registry::{VmGetter, VmRegistry};
pub use subnet::{PRIMARY_NETWORK_ID, Subnet, SubnetConfig};

#[cfg(test)]
mod dev_deps {
    // Dev-dependencies exercised only by the integration tests under `tests/`;
    // reference them so the unit-test build of the lib does not warn about
    // unused dev-deps.
    use assert_matches as _;
    use proptest as _;
    use tokio as _;
}
