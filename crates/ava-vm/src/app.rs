// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The VM's inbound app-message side and the typed [`AppError`]
//! (`snow/engine/common`, specs 07 §2.2).

use std::time::Instant;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;

use crate::error::Result;

/// `snow/engine/common.AppHandler` — the VM's inbound app-message side.
///
/// Ports `AppRequestHandler` + `AppResponseHandler` + `AppGossipHandler`. Go's
/// `context.Context` becomes a `&CancellationToken`; the `time.Time` deadline on
/// `AppRequest` becomes [`std::time::Instant`].
#[async_trait]
pub trait AppHandler: Send + Sync {
    /// Notify the VM of a request for an `AppResponse` with the same
    /// `request_id`. The meaning of `request` is VM-specific and not guaranteed
    /// to be well-formed or valid. May be called by any node at any time.
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> Result<()>;

    /// Notify the VM that an `AppRequest` it issued has failed (the peer will
    /// not respond). `err` carries the application-level [`AppError`].
    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> Result<()>;

    /// Notify the VM of the response to a previously sent `AppRequest` with the
    /// same `request_id`. The meaning of `response` is VM-specific.
    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> Result<()>;

    /// Notify the VM of a gossip message from `node`. Not expected in response
    /// to any event and does not need to be responded to.
    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> Result<()>;
}

/// `snow/engine/common.AppError` — an application-defined error carried across
/// the app-message and gRPC boundaries (specs 07 §2.2, §9).
///
/// This is a **separate** error type from the crate [`Error`](crate::error::Error):
/// it is matched by its integer `code` (not by structural variant), mirroring
/// Go's `(*AppError).Is`, which compares only `Code`. The predefined codes keep
/// the exact Go integer values so they round-trip over `proto/vm`/`proto/appsender`.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AppError {
    /// Application-defined error code, used for matching. Negative codes are
    /// reserved by the framework ([`AppError::TIMEOUT`]).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
}

impl AppError {
    /// `ErrUndefined.Code` — an undefined application error.
    pub const UNDEFINED: i32 = 0;
    /// `ErrTimeout.Code` — signals an `AppRequest` response timeout.
    pub const TIMEOUT: i32 = -1;

    /// Constructs an `AppError` from a code and message.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `ErrUndefined` — the predefined `code == 0` error.
    #[must_use]
    pub fn undefined() -> Self {
        Self::new(Self::UNDEFINED, "undefined")
    }

    /// `ErrTimeout` — the predefined `code == -1` error.
    #[must_use]
    pub fn timeout() -> Self {
        Self::new(Self::TIMEOUT, "timed out")
    }

    /// `(*AppError).Is` — two `AppError`s are considered equal iff their codes
    /// match (the message is ignored), matching Go's sentinel comparison.
    #[must_use]
    pub fn is(&self, other: &AppError) -> bool {
        self.code == other.code
    }
}
