// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Node::new` initialization steps (specs/12 §2.2, mirror
//! `node/node.go::New`).
//!
//! One module per init concern; [`crate::node::Node::new`] calls them in the
//! exact Go order (the 26-step sequence asserted by
//! `node::tests::init_order_matches_go`). Where a downstream subsystem does
//! not yet expose what Go's init step needs, the module defines a **narrow
//! seam** local to `ava-node` (documented in `crates/ava-node/tests/PORTING.md`)
//! rather than refactoring the other crate.

use std::sync::Arc;

pub mod aliases;
pub mod api_server;
pub mod api_services;
pub mod bootstrappers;
pub mod chain_manager;
pub mod database;
pub mod db_init;
pub mod dispatchers;
pub mod health;
pub mod identity;
pub mod message;
pub mod metrics;
pub mod nat;
pub mod networking;
pub mod resource;
pub mod validators;
pub mod vms;

/// A request to shut the node down with an exit code. `Node::new` hands one of
/// these to the subsystems that can demand a shutdown before `Node` itself
/// exists (the indexer's fatal close, the disk-space health check). In M8.29
/// the trigger records the exit code and cancels the root
/// [`tokio_util::sync::CancellationToken`]; the full 14-step shutdown sequence
/// lands in M8.30 (specs/12 §2.4).
pub type ShutdownTrigger = Arc<dyn Fn(i32) + Send + Sync>;

/// The Go metric-namespace constants from `node/node.go` (18 §1.2): each is
/// `avalanche_<suffix>` via [`ava_api::metrics::append_namespace`].
pub(crate) mod namespace {
    use ava_api::metrics::{PLATFORM_NAME, append_namespace};

    /// `apiNamespace`.
    pub(crate) fn api() -> String {
        append_namespace(PLATFORM_NAME, "api")
    }
    /// `benchlistNamespace`.
    pub(crate) fn benchlist() -> String {
        append_namespace(PLATFORM_NAME, "benchlist")
    }
    /// `dbNamespace`.
    pub(crate) fn db() -> String {
        append_namespace(PLATFORM_NAME, "db")
    }
    /// `healthNamespace`.
    pub(crate) fn health() -> String {
        append_namespace(PLATFORM_NAME, "health")
    }
    /// `meterDBNamespace`.
    pub(crate) fn meterdb() -> String {
        append_namespace(PLATFORM_NAME, "meterdb")
    }
    /// `networkNamespace`.
    pub(crate) fn network() -> String {
        append_namespace(PLATFORM_NAME, "network")
    }
    /// `processNamespace`.
    pub(crate) fn process() -> String {
        append_namespace(PLATFORM_NAME, "process")
    }
    /// `requestsNamespace`.
    pub(crate) fn requests() -> String {
        append_namespace(PLATFORM_NAME, "requests")
    }
    /// `resourceTrackerNamespace`.
    pub(crate) fn resource_tracker() -> String {
        append_namespace(PLATFORM_NAME, "resource_tracker")
    }
    /// `responsesNamespace`.
    pub(crate) fn responses() -> String {
        append_namespace(PLATFORM_NAME, "responses")
    }
    /// `rpcchainvmNamespace`.
    pub(crate) fn rpcchainvm() -> String {
        append_namespace(PLATFORM_NAME, "rpcchainvm")
    }
    /// `systemResourcesNamespace`.
    pub(crate) fn system_resources() -> String {
        append_namespace(PLATFORM_NAME, "system_resources")
    }
    /// `upgradeNamespace`.
    pub(crate) fn upgrade() -> String {
        append_namespace(PLATFORM_NAME, "upgrade")
    }
}
