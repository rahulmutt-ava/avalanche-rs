// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! VM framework base traits (`specs/07-vm-framework.md` §2.1, §2.2, §2.6, §9).
//!
//! This crate defines the contract the consensus engine (`ava-snow`/`ava-engine`)
//! drives and the VMs (`ava-platformvm`/`ava-avm`/`ava-evm`/`ava-saevm`)
//! implement. The base surface ported here:
//!
//! * [`Vm`] — `snow/engine/common.VM`, the base every VM implements, with
//!   supertraits [`AppHandler`], [`HealthCheck`], and [`Connector`].
//! * [`VmEvent`] — `common.Message` (`PendingTxs`/`StateSyncDone`).
//! * [`HttpHandler`]/[`LockOptions`] — `common.HTTPHandler` descriptor.
//! * [`AppHandler`]/[`AppError`] — the inbound app-message side + typed error.
//! * [`HealthCheck`] — `api/health.Checker`.
//! * [`Connector`] — `snow/validators.Connector`.
//! * [`AppSender`]/[`SendConfig`] — the outbound app handle (`common.AppSender`).
//! * [`Error`]/[`Result`] — the crate error model with preserved Go sentinels.
//!
//! Async traits use `async_trait` and replace Go's `context.Context` with a
//! `&tokio_util::sync::CancellationToken`. `ChainContext`/`EngineState` are
//! re-exported from `ava-snow` (specs 06 §3).

#![forbid(unsafe_code)]

pub mod app;
pub mod app_sender;
pub mod block;
pub mod components;
pub mod connector;
pub mod error;
pub mod fx;
pub mod health;
pub mod middleware;
pub mod vm;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use app::{AppError, AppHandler};
pub use app_sender::{AppSender, SendConfig};
pub use block::{
    BatchedChainVm, Block, BlockContext, BuildBlockWithContext, ChainVm, INT_LEN,
    SetPreferenceWithContext, StateSummary, StateSyncMode, StateSyncableVm, WithVerifyContext,
    batched_parse_block, get_ancestors,
};
pub use connector::Connector;
pub use error::{Error, Result};
pub use fx::{CodecRegistry, Fx, FxInstance, FxVm, UnsignedTx};
pub use health::HealthCheck;
pub use middleware::{BlockMetrics, MeterVm, TracedVm};
pub use vm::{HttpHandler, LockOptions, Vm, VmEvent, VmHttpService, VmRequest, VmResponse};

// Re-export the consensus context + engine phase the VM consumes at the
// boundary (specs 06 §3), so downstream VM crates depend only on `ava-vm`.
pub use ava_snow::{ChainContext, ConsensusContext, EngineState};
// Re-exported so the `vm_conformance!` macro can name `$crate::Id` /
// `$crate::EngineState` hygienically from a downstream crate (07 §10).
pub use ava_types::id::Id;

#[cfg(test)]
mod tests {
    // `proptest` is a declared dev-dependency reserved for the VM-conformance
    // proptests (specs 07 §10); `ava-secp256k1fx` is a dev-only edge exercised
    // solely by `tests/fx.rs` (M3.20). Silence `unused_crate_dependencies` for
    // the lib-test unit, which references neither.
    use assert_matches::assert_matches;
    use ava_secp256k1fx as _;
    use proptest as _;

    use super::*;

    /// `common.Message` discriminants must match Go's `iota + 1`.
    #[test]
    fn vm_event_values() {
        assert_eq!(VmEvent::PendingTxs as u32, 1);
        assert_eq!(VmEvent::StateSyncDone as u32, 2);
    }

    /// The preserved Go sentinels exist and are `matches!`-assertable, and the
    /// typed `AppError` keeps Go's integer codes + `Is`-by-code semantics.
    #[test]
    fn error_sentinels() {
        // database / lookup sentinel
        assert_matches!(Error::NotFound, Error::NotFound);
        // rpcchainvm host/guest sentinels
        assert_matches!(Error::RemoteVmNotImplemented, Error::RemoteVmNotImplemented);
        assert_matches!(
            Error::StateSyncableVmNotImplemented,
            Error::StateSyncableVmNotImplemented
        );
        assert_matches!(
            Error::ProtocolVersionMismatch,
            Error::ProtocolVersionMismatch
        );
        assert_matches!(Error::HandshakeFailed, Error::HandshakeFailed);
        assert_matches!(Error::ProcessNotFound, Error::ProcessNotFound);
        // fx wrong-type set (sample of the family ava-secp256k1fx re-exports)
        assert_matches!(Error::WrongVmType, Error::WrongVmType);
        assert_matches!(Error::WrongSig, Error::WrongSig);
        assert_matches!(Error::AddrsNotSortedUnique, Error::AddrsNotSortedUnique);

        // AppError: codes match Go, Is compares only by code.
        assert_eq!(AppError::UNDEFINED, 0);
        assert_eq!(AppError::TIMEOUT, -1);
        assert_eq!(AppError::undefined().code, 0);
        assert_eq!(AppError::timeout().code, -1);
        let a = AppError::new(7, "a");
        let b = AppError::new(7, "different message");
        let c = AppError::new(8, "a");
        assert!(a.is(&b));
        assert!(!a.is(&c));
    }

    /// The in-process HTTP seam (M8.22): header lookup is case-insensitive and
    /// multiplicity-preserving (Go `r.Header[key]` is a `[]string`), and the
    /// `HttpHandler` constructors set the transport fields as documented.
    #[test]
    fn vm_request_headers_and_http_handler() {
        use std::sync::Arc;

        let req = VmRequest {
            method: "POST".to_string(),
            uri: "/".to_string(),
            headers: vec![
                ("Avalanche-Api-Route".to_string(), "chain".to_string()),
                ("avalanche-api-route".to_string(), "proposervm".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
            ],
            body: Vec::new(),
        };
        let values: Vec<&str> = req.header_values("AVALANCHE-API-ROUTE").collect();
        assert_eq!(values, ["chain", "proposervm"], "VmRequest::header_values");
        assert_eq!(
            req.header("content-type"),
            Some("application/json"),
            "VmRequest::header"
        );

        struct S;
        #[async_trait::async_trait]
        impl VmHttpService for S {
            async fn serve_http(&self, _req: VmRequest) -> VmResponse {
                VmResponse::ok("text/plain", b"ok".to_vec())
            }
        }
        let svc: Arc<dyn VmHttpService> = Arc::new(S);
        let wire = HttpHandler::new(LockOptions::WriteLock, vec![1]);
        assert_eq!(wire.service.as_ref().map(|_| ()), None, "HttpHandler::new");
        let in_proc = HttpHandler::in_process(LockOptions::NoLock, svc.clone());
        assert!(in_proc.handler.is_empty(), "HttpHandler::in_process");
        // Identity-based equality on the in-process service.
        assert_eq!(
            in_proc,
            HttpHandler::in_process(LockOptions::NoLock, svc),
            "HttpHandler PartialEq (same Arc)"
        );
        assert_ne!(wire, in_proc, "HttpHandler PartialEq (wire vs in-process)");
    }

    /// `Vm` (and its supertraits) must be object-safe so the engine can hold
    /// `Arc<dyn Vm>`.
    #[test]
    fn vm_object_safe() {
        fn _o(_: &dyn Vm) {}
        fn _app(_: &dyn AppHandler) {}
        fn _health(_: &dyn HealthCheck) {}
        fn _conn(_: &dyn Connector) {}
        fn _sender(_: &dyn AppSender) {}
    }
}
