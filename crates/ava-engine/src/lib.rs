// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-engine` — the consensus-engine framework (port of `snow/engine/common`,
//! specs 06 §4).
//!
//! This crate defines the inbound-op **state machine** every consensus engine
//! implements: one object-safe `#[async_trait]` trait per op group (state-sync,
//! frontier, accepted, ancestors, get/put, query, chits, app, internal,
//! simplex), composed into the object-safe [`Handler`]; the
//! [`Engine`]`: Handler` super-trait adding `start`/`health_check`; the
//! log-and-drop [`NoOpHandler`] default mixin; the typed [`AppError`] carried on
//! `AppRequestFailed`/`SendAppError`; and the engine-facing [`Sender`] +
//! [`SendConfig`].
//!
//! All node IDs reaching a handler are **pre-authenticated** by the network
//! layer (specs 05).

#![forbid(unsafe_code)]

pub mod common;
pub mod error;

pub use common::engine::Engine;
pub use common::error::AppError;
pub use common::handler::{
    AcceptedHandler, AllGetsServer, AncestorsHandler, AppHandler, ChitsHandler, FrontierHandler,
    Handler, InternalHandler, PutHandler, QueryHandler, SimplexHandler, StateSyncHandler,
};
pub use common::no_ops::NoOpHandler;
pub use common::sender::{SendConfig, Sender};
pub use error::{Error, Result};

#[cfg(test)]
mod tests {
    use ava_types::node_id::NodeId;
    use ava_vm::VmEvent;

    use super::common::handler::Handler;
    use super::common::no_ops::NoOpHandler;
    use super::AppError;

    /// `app_error_codes` — predefined codes and `Is`-by-code semantics.
    #[test]
    fn app_error_codes() {
        assert_eq!(AppError::UNDEFINED, 0);
        assert_eq!(AppError::TIMEOUT, -1);

        let undefined = AppError::undefined();
        let timeout = AppError::timeout();
        assert_eq!(undefined.code, 0);
        assert_eq!(timeout.code, -1);

        // `Is` matches purely by code, ignoring the message (mirrors Go).
        let same_code_diff_msg = AppError::new(0, "a totally different message");
        assert!(undefined.is(&same_code_diff_msg));
        assert!(!undefined.is(&timeout));

        // A custom application code does not match a framework code.
        let custom = AppError::new(42, "boom");
        assert!(!custom.is(&undefined));
        assert!(custom.is(&AppError::new(42, "different")));
    }

    /// `handler_is_object_safe` — static-assert `dyn Handler` is usable.
    #[test]
    fn handler_is_object_safe() {
        fn _o(_: &dyn Handler) {}
        // Box form too, to exercise the full object-safety requirement.
        fn _b(_: Box<dyn Handler>) {}
    }

    /// `noop_handler_drops_statesync` — a `NoOpHandler`-backed type returns
    /// `Ok(())` for the state-summary ops.
    #[tokio::test]
    async fn noop_handler_drops_statesync() {
        use super::common::handler::{InternalHandler, StateSyncHandler};

        let mut h = NoOpHandler;
        let node = NodeId::from([7u8; 20]);

        h.get_state_summary_frontier(node, 1).await.unwrap();
        h.state_summary_frontier(node, 1, &[1, 2, 3]).await.unwrap();
        h.get_state_summary_frontier_failed(node, 1).await.unwrap();
        h.get_accepted_state_summary(node, 2, &[10, 20]).await.unwrap();
        h.accepted_state_summary(node, 2, &[]).await.unwrap();
        h.get_accepted_state_summary_failed(node, 2).await.unwrap();

        // The internal notify path also drops cleanly.
        h.notify(VmEvent::StateSyncDone).await.unwrap();
    }
}
