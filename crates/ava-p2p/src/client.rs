// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Client` — per-handler request/response correlation (Go `network/p2p/client.go`
//! `Client`).
//!
//! ## Register-happens-before-send
//!
//! Go's `Client.AppRequest` runs its whole body — id allocation, the
//! (synchronous) `SendAppRequest` call, and the pending-map insert — under
//! `c.router.lock` (`network/p2p/client.go:70-107`), so Go's cooperative,
//! non-preemptive scheduling guarantees no other goroutine can observe an
//! `AppResponse`/`AppRequestFailed` for a request id before that id's entry
//! exists in `pendingAppRequests`, even though the map insert
//! (`client.go:104-106`) textually follows the send.
//!
//! This port's [`AppSender::send_app_request`] is `async` and may genuinely
//! yield to the executor mid-call, so a literal transliteration (insert the
//! pending entry only *after* `send_app_request`'s future resolves) would
//! open a window where a fast peer's `AppResponse`/`AppRequestFailed` — routed
//! through a different task calling
//! [`P2pNetwork::handle_app_response`](crate::network::P2pNetwork::handle_app_response) —
//! arrives and finds nothing to correlate against. [`Client::app_request`]
//! below instead inserts `on_response` into the pending map *before* awaiting
//! `send_app_request`, the same register-happens-before-send ordering used
//! elsewhere in this port (Task 3's `network.rs` module doc references the
//! same "STEP-p" pattern).
//!
//! ## Narrowed from Go's multi-node fan-out
//!
//! Go's `Client.AppRequest` takes a `set.Set[ids.NodeID]` and fans one logical
//! request out to every member, incrementing the shared `requestID` by 2 per
//! node so each gets its own id (`client.go:79-108`); `Client.AppRequestAny`
//! additionally samples a single node via a `NodeSampler`. This port's
//! [`Client::app_request`] takes a single `node: NodeId` per call — the Task 4
//! brief's interface — so multi-node fan-out is simply one `app_request` call
//! per node from the caller; node sampling, if needed, is
//! [`P2pNetwork::sample_peer`](crate::network::P2pNetwork::sample_peer).
//!
//! ## Request id allocation
//!
//! Go reserves the SDK's request ids to odd numbers only (`router.go:83-84`,
//! `invariant: sdk uses odd-numbered requestIDs`, `requestID: 1`, `+= 2` per
//! allocation) because the id space is shared with the core engine's own
//! (even-numbered) request ids on the same wire. This port's `P2pNetwork` has
//! no separate core-engine request-id consumer sharing its space, so
//! [`Client::app_request`] allocates plain sequential ids
//! (`request_id.fetch_add(1, ...)`) starting at 0 — there is nothing else to
//! collide with.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;
use ava_vm::app::AppError;
use ava_vm::app_sender::{AppSender, SendConfig};

use crate::network::protocol_prefix;

/// Invoked exactly once with the responding node and either the response
/// bytes or the [`AppError`] the request failed with (Go's
/// `AppResponseCallback`, `network/p2p/client.go:24-29`).
///
/// A `FnOnce` rather than Go's plain (re-invocable-in-principle) function
/// value: [`P2pNetwork`](crate::network::P2pNetwork) already guarantees
/// exactly-once delivery per request id by removing the pending entry before
/// invoking it (see [`crate::network::P2pNetwork::handle_app_response`]), and
/// `FnOnce` lets Rust enforce that the callback itself cannot be called
/// twice.
pub type OnResponse = Box<dyn FnOnce(NodeId, Result<Vec<u8>, AppError>) + Send>;

/// Shared map of in-flight request ids to their pending callback (Go
/// `router.pendingAppRequests`, `network/p2p/router.go`).
///
/// One `PendingMap` is shared by a `P2pNetwork` and every [`Client`] issued
/// off it via [`P2pNetwork::add_handler`](crate::network::P2pNetwork::add_handler)
/// (Go: every `Client` for a network points at the same `*router`), so a
/// response routed through the network's `handle_app_response` can resolve a
/// callback registered by any of that network's `Client`s.
pub(crate) type PendingMap = Arc<Mutex<std::collections::HashMap<u32, OnResponse>>>;

/// Issues correlated `AppRequest`s and fire-and-forget `AppGossip`s on behalf
/// of one registered handler (Go `network/p2p/client.go` `Client`).
///
/// The only public constructor path is
/// [`P2pNetwork::add_handler`](crate::network::P2pNetwork::add_handler),
/// mirroring Go's `Network.NewClient` being the sole way to obtain a
/// `*Client`. Every `Client` returned for a given `P2pNetwork` shares that
/// network's pending-request map and request-id counter.
pub struct Client {
    handler_id: u64,
    prefix: Vec<u8>,
    sender: Arc<dyn AppSender>,
    pending: PendingMap,
    request_id: Arc<AtomicU32>,
}

impl std::fmt::Debug for Client {
    /// Prints only `handler_id`/`prefix` — `sender` (`Arc<dyn AppSender>`) and
    /// the pending map's boxed `OnResponse` callbacks have no meaningful
    /// `Debug` representation.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("handler_id", &self.handler_id)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Constructs a `Client` for `handler_id`, sharing `sender`, `pending`,
    /// and `request_id` with the owning `P2pNetwork`. `pub(crate)` because
    /// [`P2pNetwork::add_handler`](crate::network::P2pNetwork::add_handler)
    /// is the only sanctioned way to obtain a `Client` — see the type doc.
    pub(crate) fn new(
        handler_id: u64,
        sender: Arc<dyn AppSender>,
        pending: PendingMap,
        request_id: Arc<AtomicU32>,
    ) -> Self {
        Self {
            handler_id,
            prefix: protocol_prefix(handler_id),
            sender,
            pending,
            request_id,
        }
    }

    /// Returns the handler id this `Client` issues requests/gossip for.
    #[must_use]
    pub fn handler_id(&self) -> u64 {
        self.handler_id
    }

    /// Issues an `AppRequest` to `node`, prefixed with this handler's id (Go
    /// `Client.AppRequest`, `network/p2p/client.go:64-107`, narrowed to a
    /// single node — see the module doc).
    ///
    /// Allocates the next id off the shared request-id counter and registers
    /// `on_response` in the pending map *before* awaiting
    /// [`AppSender::send_app_request`] (see the module doc's
    /// "register-happens-before-send" section). If the send itself returns an
    /// error, the just-inserted pending entry is removed and the error is
    /// returned to the caller directly — `on_response` is not invoked,
    /// mirroring Go returning the `SendAppRequest` error straight to the
    /// caller rather than resolving the callback for a request that never
    /// went out.
    pub async fn app_request(
        &self,
        token: &CancellationToken,
        node: NodeId,
        bytes: Vec<u8>,
        on_response: OnResponse,
    ) -> crate::Result<()> {
        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed);
        // Register before the send even starts (not merely before its await
        // resolves) so a same-thread synchronous fast-path response can never
        // race ahead of the insert either.
        self.pending.lock().insert(request_id, on_response);

        let mut nodes = HashSet::with_capacity(1);
        nodes.insert(node);
        let prefixed = prefix_message(&self.prefix, &bytes);

        if let Err(err) = self
            .sender
            .send_app_request(token, &nodes, request_id, prefixed)
            .await
        {
            self.pending.lock().remove(&request_id);
            return Err(crate::Error::Send(err.to_string()));
        }

        Ok(())
    }

    /// Sends a fire-and-forget `AppGossip` to the peers selected by `config`,
    /// prefixed with this handler's id (Go `Client.AppGossip`,
    /// `network/p2p/client.go:112-127`).
    pub async fn app_gossip(
        &self,
        token: &CancellationToken,
        config: SendConfig,
        bytes: Vec<u8>,
    ) -> crate::Result<()> {
        let prefixed = prefix_message(&self.prefix, &bytes);
        self.sender
            .send_app_gossip(token, config, prefixed)
            .await
            .map_err(|err| crate::Error::Send(err.to_string()))
    }
}

/// Prefixes `msg` with `prefix` (Go `network/p2p/client.go` `PrefixMessage`).
fn prefix_message(prefix: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len().saturating_add(msg.len()));
    out.extend_from_slice(prefix);
    out.extend_from_slice(msg);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use async_trait::async_trait;
    use ava_vm::error::Result as VmResult;

    use crate::handler::Handler;
    use crate::network::{P2pNetwork, protocol_prefix};

    use super::*;

    const TEST_HANDLER_ID: u64 = 5;

    /// One recorded `send_app_request` call's arguments.
    type RecordedRequest = (HashSet<NodeId>, u32, Vec<u8>);

    /// Records every `send_app_request` call it receives; every method
    /// otherwise succeeds trivially (mirrors `network.rs`'s `RecordingSender`,
    /// duplicated here rather than shared since it's test-only and each
    /// module's variant records what that module's tests need).
    #[derive(Default)]
    struct RecordingSender {
        requests: StdMutex<Vec<RecordedRequest>>,
    }

    #[async_trait]
    impl AppSender for RecordingSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            nodes: &HashSet<NodeId>,
            request_id: u32,
            bytes: Vec<u8>,
        ) -> VmResult<()> {
            self.requests
                .lock()
                .unwrap()
                .push((nodes.clone(), request_id, bytes));
            Ok(())
        }

        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }

        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> VmResult<()> {
            Ok(())
        }

        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }
    }

    /// A sender whose `send_app_request` always fails, to exercise the
    /// pending-entry-cleanup path in [`Client::app_request`].
    #[derive(Default)]
    struct FailingSender;

    #[async_trait]
    impl AppSender for FailingSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            _nodes: &HashSet<NodeId>,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Err(ava_vm::error::Error::NotFound)
        }

        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }

        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            _node: NodeId,
            _request_id: u32,
            _code: i32,
            _message: &str,
        ) -> VmResult<()> {
            Ok(())
        }

        async fn send_app_gossip(
            &self,
            _token: &CancellationToken,
            _config: SendConfig,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }
    }

    struct NoopHandler;

    #[async_trait]
    impl Handler for NoopHandler {
        async fn app_gossip(&self, _node: NodeId, _msg: &[u8]) {}

        async fn app_request(
            &self,
            _node: NodeId,
            _deadline: Instant,
            _msg: &[u8],
        ) -> Result<Vec<u8>, AppError> {
            Ok(Vec::new())
        }
    }

    fn test_node(byte: u8) -> NodeId {
        NodeId::from([byte; 20])
    }

    type Delivery = (NodeId, Result<Vec<u8>, AppError>);

    #[tokio::test]
    async fn client_correlates_response() {
        let sender = Arc::new(RecordingSender::default());
        let network = P2pNetwork::new(test_node(0), sender.clone());
        let client = network
            .add_handler(TEST_HANDLER_ID, Arc::new(NoopHandler))
            .unwrap();

        let token = CancellationToken::new();
        let node = test_node(1);

        // --- (d) request bytes on the wire carry the varint prefix, and (a)
        // the callback fires exactly once with the Ok payload. ---
        let ok_deliveries: Arc<StdMutex<Vec<Delivery>>> = Arc::new(StdMutex::new(Vec::new()));
        let ok_deliveries_clone = ok_deliveries.clone();
        client
            .app_request(
                &token,
                node,
                b"req".to_vec(),
                Box::new(move |n, res| {
                    ok_deliveries_clone.lock().unwrap().push((n, res));
                }),
            )
            .await
            .unwrap();

        let request_id = {
            let requests = sender.requests.lock().unwrap();
            assert_eq!(requests.len(), 1, "send_app_request call count");
            let (nodes, request_id, bytes) = requests.first().unwrap();
            assert_eq!(*nodes, HashSet::from([node]), "send_app_request nodes");
            let mut expected = protocol_prefix(TEST_HANDLER_ID);
            expected.extend_from_slice(b"req");
            assert_eq!(*bytes, expected, "send_app_request bytes carry the prefix");
            *request_id
        };

        network
            .handle_app_response(&token, node, request_id, b"resp")
            .await
            .unwrap();
        {
            let deliveries = ok_deliveries.lock().unwrap();
            assert_eq!(deliveries.len(), 1, "callback fires exactly once");
            let (got_node, got_result) = deliveries.first().unwrap();
            assert_eq!(*got_node, node, "callback node");
            assert_eq!(got_result.as_ref().unwrap(), b"resp", "callback Ok payload");
        }

        // (c) a second delivery for the same (now-removed) id is a no-op.
        network
            .handle_app_response(&token, node, request_id, b"resp-again")
            .await
            .unwrap();
        assert_eq!(
            ok_deliveries.lock().unwrap().len(),
            1,
            "duplicate response must not re-invoke the callback"
        );

        // --- (b) the failure path fires Err, and is likewise deduped. ---
        let err_deliveries: Arc<StdMutex<Vec<Delivery>>> = Arc::new(StdMutex::new(Vec::new()));
        let err_deliveries_clone = err_deliveries.clone();
        client
            .app_request(
                &token,
                node,
                b"req2".to_vec(),
                Box::new(move |n, res| {
                    err_deliveries_clone.lock().unwrap().push((n, res));
                }),
            )
            .await
            .unwrap();
        let request_id_2 = sender.requests.lock().unwrap().get(1).unwrap().1;
        assert_ne!(
            request_id_2, request_id,
            "each app_request gets a fresh request id"
        );

        let failure = AppError::new(-7, "boom");
        network
            .handle_app_request_failed(&token, node, request_id_2, failure.clone())
            .await
            .unwrap();
        {
            let deliveries = err_deliveries.lock().unwrap();
            assert_eq!(deliveries.len(), 1, "failure callback fires exactly once");
            let (got_node, got_result) = deliveries.first().unwrap();
            assert_eq!(*got_node, node, "failure callback node");
            let got_err = got_result.as_ref().unwrap_err();
            assert!(
                got_err.is(&failure),
                "failure callback carries the AppError"
            );
        }

        // Second delivery for the same id is a no-op for the failure path too.
        network
            .handle_app_request_failed(&token, node, request_id_2, failure)
            .await
            .unwrap();
        assert_eq!(
            err_deliveries.lock().unwrap().len(),
            1,
            "duplicate failure must not re-invoke the callback"
        );
    }

    #[tokio::test]
    async fn response_for_unknown_request_id_is_dropped_silently() {
        let sender = Arc::new(RecordingSender::default());
        let network = P2pNetwork::new(test_node(0), sender);
        let token = CancellationToken::new();

        // No app_request was ever issued for id 42; this must not panic and
        // must return Ok (Go: the router's timeout synthesis racing a real
        // reply is expected, not an error condition).
        network
            .handle_app_response(&token, test_node(1), 42, b"stray")
            .await
            .unwrap();
        network
            .handle_app_request_failed(&token, test_node(1), 42, AppError::timeout())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn app_request_removes_pending_entry_on_send_failure() {
        let sender = Arc::new(FailingSender);
        let network = P2pNetwork::new(test_node(0), sender);
        let client = network
            .add_handler(TEST_HANDLER_ID, Arc::new(NoopHandler))
            .unwrap();
        let token = CancellationToken::new();
        let node = test_node(1);

        let called = Arc::new(StdMutex::new(false));
        let called_clone = called.clone();
        let err = client
            .app_request(
                &token,
                node,
                b"req".to_vec(),
                Box::new(move |_, _| {
                    *called_clone.lock().unwrap() = true;
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::Send(_)));
        assert!(
            !*called.lock().unwrap(),
            "on_response must not fire when the send itself fails"
        );

        // The pending entry was cleaned up, not leaked: a stray response for
        // request id 0 (the only id ever allocated here) is dropped silently
        // rather than resolving the (already-failed) callback again.
        network
            .handle_app_response(&token, node, 0, b"late")
            .await
            .unwrap();
        assert!(!*called.lock().unwrap());
    }

    #[tokio::test]
    async fn app_gossip_prefixes_bytes() {
        struct GossipRecordingSender {
            gossips: StdMutex<Vec<Vec<u8>>>,
        }

        #[async_trait]
        impl AppSender for GossipRecordingSender {
            async fn send_app_request(
                &self,
                _token: &CancellationToken,
                _nodes: &HashSet<NodeId>,
                _request_id: u32,
                _bytes: Vec<u8>,
            ) -> VmResult<()> {
                Ok(())
            }

            async fn send_app_response(
                &self,
                _token: &CancellationToken,
                _node: NodeId,
                _request_id: u32,
                _bytes: Vec<u8>,
            ) -> VmResult<()> {
                Ok(())
            }

            async fn send_app_error(
                &self,
                _token: &CancellationToken,
                _node: NodeId,
                _request_id: u32,
                _code: i32,
                _message: &str,
            ) -> VmResult<()> {
                Ok(())
            }

            async fn send_app_gossip(
                &self,
                _token: &CancellationToken,
                _config: SendConfig,
                bytes: Vec<u8>,
            ) -> VmResult<()> {
                self.gossips.lock().unwrap().push(bytes);
                Ok(())
            }
        }

        let sender = Arc::new(GossipRecordingSender {
            gossips: StdMutex::new(Vec::new()),
        });
        let network = P2pNetwork::new(test_node(0), sender.clone());
        let client = network
            .add_handler(TEST_HANDLER_ID, Arc::new(NoopHandler))
            .unwrap();
        let token = CancellationToken::new();

        client
            .app_gossip(&token, SendConfig::default(), b"gossip".to_vec())
            .await
            .unwrap();

        let gossips = sender.gossips.lock().unwrap();
        assert_eq!(gossips.len(), 1);
        let mut expected = protocol_prefix(TEST_HANDLER_ID);
        expected.extend_from_slice(b"gossip");
        assert_eq!(gossips.first().unwrap(), &expected);
    }
}
