// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-protocol application handler (Go `network/p2p/handler.go`) + the
//! standardized `AppError`s (Go `network/p2p/error.go`).

use std::time::Instant;

use async_trait::async_trait;

use ava_types::node_id::NodeId;
use ava_vm::app::AppError;

/// A per-protocol application handler (Go `network/p2p/handler.go` `Handler`).
///
/// [`P2pNetwork`](crate::network::P2pNetwork) dispatches varint-prefixed
/// `AppGossip`/`AppRequest` payloads to the `Handler` registered under the
/// matching handler id, mirroring Go's `responder` (`network/p2p/network.go`).
#[async_trait]
pub trait Handler: Send + Sync {
    /// Handle a gossip payload (prefix already stripped). Errors are dropped
    /// by the caller (gossip is fire-and-forget).
    async fn app_gossip(&self, node: NodeId, msg: &[u8]);
    /// Handle a request payload; `Ok` bytes become the `AppResponse`,
    /// `Err(AppError)` becomes the `AppError` reply.
    async fn app_request(
        &self,
        node: NodeId,
        deadline: Instant,
        msg: &[u8],
    ) -> Result<Vec<u8>, AppError>;
}

/// Standardized tx-gossip handler id (Go `network/p2p/network.go:25-29`
/// iota's first value). This port only defines the two ids consumed by the
/// C-Chain tx gossip milestone; Go's `SignatureRequestHandlerID` (ACP-118) and
/// `FirewoodProofHandlerID` are out of scope here.
pub const TX_GOSSIP_HANDLER_ID: u64 = 0;
/// Atomic tx gossip handler id (out of scope this milestone; reserved so the
/// id space matches Go's iota ordering).
pub const ATOMIC_TX_GOSSIP_HANDLER_ID: u64 = 1;

/// `ErrUnexpected` (Go `network/p2p/error.go`) — a request failed due to a
/// generic error.
#[must_use]
pub fn err_unexpected() -> AppError {
    AppError::new(-1, "unexpected error")
}

/// `ErrUnregisteredHandler` (Go `network/p2p/error.go`) — a request failed
/// because it did not match a registered handler.
#[must_use]
pub fn err_unregistered_handler() -> AppError {
    AppError::new(-2, "unregistered handler")
}

/// `ErrNotValidator` (Go `network/p2p/error.go`) — a request failed because
/// the requesting peer is not a validator.
#[must_use]
pub fn err_not_validator() -> AppError {
    AppError::new(-3, "not a validator")
}

/// `ErrThrottled` (Go `network/p2p/error.go`) — a request failed because the
/// requesting peer exceeded a rate limit.
#[must_use]
pub fn err_throttled() -> AppError {
    AppError::new(-4, "throttled")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_constructors_match_go_codes_and_messages() {
        let cases = [
            (err_unexpected(), -1, "unexpected error"),
            (err_unregistered_handler(), -2, "unregistered handler"),
            (err_not_validator(), -3, "not a validator"),
            (err_throttled(), -4, "throttled"),
        ];
        for (err, code, message) in cases {
            assert_eq!(err.code, code);
            assert_eq!(err.message, message);
        }
    }
}
