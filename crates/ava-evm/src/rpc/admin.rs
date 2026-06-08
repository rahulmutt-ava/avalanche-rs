// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `admin.*` JSON-RPC handlers (the C-Chain admin service, G8, spec 10 §9.2,
//! M6.24).
//!
//! Port of coreth `plugin/evm/admin.go`. Like the `avax.*` / `eth_*` handlers
//! (M6.23/M6.24), these are plain handlers returning a [`serde_json::Value`] in
//! coreth's admin-service shapes (the jsonrpsee-vs-axum mount decision is deferred
//! to the 12-node milestone, §9.2).
//!
//! coreth's admin methods are `StartCPUProfiler` / `StopCPUProfiler` /
//! `MemoryProfile` / `LockProfile` (each `api.EmptyReply` = `{}`), `SetLogLevel`
//! (`api.EmptyReply`), and `GetVMConfig` (the VM config). The profiler methods and
//! log-level setter are **no-ops** in this build (the `profiler.Profiler` /
//! dynamic logger are node-assembly concerns, M6.10/§12-node); each returns the
//! empty reply `{}` so the RPC surface and shapes match coreth.

use serde_json::{Value, json};

use crate::error::Result;

/// The `admin.*` RPC handler set (M6.24). Stateless in this build (the profiler /
/// dynamic logger are wired at node assembly, §12-node), so the handlers return
/// coreth's empty/echo replies.
#[derive(Clone, Copy, Debug, Default)]
pub struct AdminRpc;

impl AdminRpc {
    /// An admin handler (no state in this build).
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// `admin.startCPUProfiler` — start the CPU profiler. **No-op** in this build
    /// (the profiler is a node-assembly concern, §12-node); returns the empty
    /// reply `{}`.
    ///
    /// # Errors
    /// Currently infallible; returns `Result` for API symmetry with the wired
    /// profiler.
    pub fn start_cpu_profiler(&self) -> Result<Value> {
        Ok(json!({}))
    }

    /// `admin.stopCPUProfiler` — stop the CPU profiler. **No-op**; returns `{}`.
    ///
    /// # Errors
    /// Currently infallible (see [`Self::start_cpu_profiler`]).
    pub fn stop_cpu_profiler(&self) -> Result<Value> {
        Ok(json!({}))
    }

    /// `admin.memoryProfile` — write a memory profile. **No-op**; returns `{}`.
    ///
    /// # Errors
    /// Currently infallible (see [`Self::start_cpu_profiler`]).
    pub fn memory_profile(&self) -> Result<Value> {
        Ok(json!({}))
    }

    /// `admin.lockProfile` — write a mutex profile. **No-op**; returns `{}`.
    ///
    /// # Errors
    /// Currently infallible (see [`Self::start_cpu_profiler`]).
    pub fn lock_profile(&self) -> Result<Value> {
        Ok(json!({}))
    }

    /// `admin.setLogLevel` — set the chain's log level. **No-op** in this build
    /// (the dynamic logger is wired at node assembly, §12-node); validates nothing
    /// and returns the empty reply `{}` (coreth returns `api.EmptyReply`).
    ///
    /// # Errors
    /// Currently infallible; returns `Result` for API symmetry with the wired
    /// logger (which can fail to parse the level).
    pub fn set_log_level(&self, _level: &str) -> Result<Value> {
        Ok(json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_methods_return_empty_reply() {
        let admin = AdminRpc::new();
        let empty = json!({});
        assert_eq!(admin.start_cpu_profiler().expect("start"), empty);
        assert_eq!(admin.stop_cpu_profiler().expect("stop"), empty);
        assert_eq!(admin.memory_profile().expect("mem"), empty);
        assert_eq!(admin.lock_profile().expect("lock"), empty);
        assert_eq!(admin.set_log_level("debug").expect("level"), empty);
    }
}
