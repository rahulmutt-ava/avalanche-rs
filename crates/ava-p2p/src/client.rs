// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Client` — per-handler request/response correlation (Go `network/p2p/client.go`
//! `Client`).
//!
//! ## Register-happens-before-send
//!
//! Go's `Client.AppRequest` runs its whole body — id allocation, the
//! (synchronous) `SendAppRequest` call, and the pending-map insert — while
//! holding `c.router.lock` for the entire critical section
//! (`network/p2p/client.go:70-107` acquires it on entry and only releases it
//! via `defer` on return; `router.pendingAppRequests`/`router.requestID` are
//! themselves declared right next to that same `lock` at `router.go:65-84`).
//! That shared mutex — not goroutine scheduling — is what makes it safe for
//! Go to insert into `pendingAppRequests` (`client.go:104-106`) *after* the
//! send: no other goroutine can call into the router's `AppResponse`/
//! `AppRequestFailed` (which also take `router.lock`, `router.go` `clearAppRequest`)
//! and observe the map mid-update.
//!
//! This port has no equivalent single lock spanning "allocate id, send,
//! insert" — [`AppSender::send_app_request`] is `async` and may genuinely
//! yield to the executor mid-call, and holding [`PendingMap`]'s
//! `parking_lot::Mutex` across that `.await` is exactly what this crate must
//! not do (a `Mutex` guard held across an await point). Without Go's
//! router-wide lock, inserting only *after* `send_app_request` resolves would
//! open a window where a fast peer's `AppResponse`/`AppRequestFailed` — routed
//! through a different task calling
//! [`P2pNetwork::handle_app_response`](crate::network::P2pNetwork::handle_app_response) —
//! arrives and finds nothing to correlate against. [`Client::app_request`]
//! below instead inserts `on_response` into the pending map *before* awaiting
//! `send_app_request` (dropping the lock guard immediately after the insert,
//! not held across the await), the same register-happens-before-send ordering
//! used elsewhere in this port (Task 3's `network.rs` module doc references
//! the same "STEP-p" pattern). [`PendingGuard`] then covers the flip side:
//! if the caller drops/cancels the `app_request` future while that send is
//! still in flight (e.g. `tokio::time::timeout`), the pending entry must not
//! outlive it either.
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

/// Removes a just-inserted pending entry on drop unless [`Self::disarm`] was
/// called first.
///
/// [`Client::app_request`] inserts `on_response` into the pending map before
/// awaiting [`AppSender::send_app_request`] (see the module doc's
/// "register-happens-before-send" section) so a fast reply can't race ahead
/// of the insert. That ordering, on its own, would leak the entry forever if
/// the caller cancels the `app_request` future while that send is still
/// in-flight — e.g. `tokio::time::timeout`, `select!`, or simply dropping the
/// future — since no later `handle_app_response`/`handle_app_request_failed`
/// call is coming to remove it. `PendingGuard` closes that window: it is
/// armed for the lifetime of the in-flight send and, if dropped while still
/// armed (early-return error *or* the surrounding future being dropped
/// mid-`.await`), removes the entry itself. A wire response/failure that
/// later arrives for an id a `PendingGuard` already cleaned up is dropped
/// silently by `P2pNetwork::handle_app_response`/`handle_app_request_failed`
/// — the same unknown/already-resolved-id policy applied to every other stray
/// delivery.
struct PendingGuard {
    pending: PendingMap,
    request_id: u32,
    armed: bool,
}

impl PendingGuard {
    /// Wraps an already-inserted `request_id`, armed to remove it on drop.
    fn new(pending: PendingMap, request_id: u32) -> Self {
        Self {
            pending,
            request_id,
            armed: true,
        }
    }

    /// Disarms the guard: the pending entry is no longer removed on drop.
    /// Call this once the request has been durably issued (the send
    /// succeeded) — from that point on, only
    /// `P2pNetwork::handle_app_response`/`handle_app_request_failed` may
    /// remove the entry.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.armed {
            self.pending.lock().remove(&self.request_id);
        }
    }
}

/// Issues correlated `AppRequest`s and fire-and-forget `AppGossip`s on behalf
/// of one registered handler (Go `network/p2p/client.go` `Client`).
///
/// The only public constructor path is
/// [`P2pNetwork::add_handler`](crate::network::P2pNetwork::add_handler),
/// mirroring Go's `Network.NewClient` being the sole way to obtain a
/// `*Client`. Every `Client` returned for a given `P2pNetwork` shares that
/// network's pending-request map and request-id counter.
///
/// `Clone` (all fields are `Arc`/`Vec<u8>`/`u64`): mirrors Go's `*Client`
/// being freely reused by value (e.g. `gossip.NewSystem`,
/// `network/p2p/gossip/system.go:151-166`, passes the SAME `client` to both
/// `NewPullGossiper` and `NewPushGossiper`) — a gossip system built from one
/// [`P2pNetwork::client`] call needs a `Client` for both its `PushGossiper`
/// and `PullGossiper`, and each constructor consumes its `Client` by value.
#[derive(Clone)]
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
    /// Allocates the next id off the shared request-id counter. If that id is
    /// still present in the pending map (Go `client.go:82-88`'s
    /// `ErrRequestPending` check, which peeks `pendingAppRequests` before
    /// issuing the send), returns [`crate::Error::RequestPending`] without
    /// touching the existing entry or sending anything — this should only be
    /// reachable after the `u32` id space wraps all the way around while very
    /// old requests are still outstanding.
    ///
    /// Otherwise, registers `on_response` in the pending map *before* awaiting
    /// [`AppSender::send_app_request`] (see the module doc's
    /// "register-happens-before-send" section), guarded by a [`PendingGuard`]
    /// that removes the entry again if the send returns an error *or* this
    /// future is dropped/cancelled before the send resolves (see
    /// `PendingGuard`'s doc). `on_response` is only ever invoked by
    /// `P2pNetwork::handle_app_response`/`handle_app_request_failed`, never
    /// from here — mirroring Go returning the `SendAppRequest` error straight
    /// to the caller rather than resolving the callback for a request that
    /// never went out.
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
        // race ahead of the insert either. The lock is held only for this
        // check-and-insert, never across the `send_app_request` await below.
        {
            let mut pending = self.pending.lock();
            if pending.contains_key(&request_id) {
                return Err(crate::Error::RequestPending(request_id));
            }
            pending.insert(request_id, on_response);
        }
        let guard = PendingGuard::new(self.pending.clone(), request_id);

        let mut nodes = HashSet::with_capacity(1);
        nodes.insert(node);
        let prefixed = prefix_message(&self.prefix, &bytes);

        // If this future is dropped while suspended on this `.await` (e.g.
        // `tokio::time::timeout` elapsing, or a `select!` losing), `guard`
        // is dropped along with it — still armed — and removes the pending
        // entry itself; nothing further to do on that path.
        self.sender
            .send_app_request(token, &nodes, request_id, prefixed)
            .await
            .map_err(|err| crate::Error::Send(err.to_string()))?;

        // The send succeeded: the entry is now durably owned by the pending
        // map, to be resolved by a later `handle_app_response`/
        // `handle_app_request_failed` call, not by this guard.
        guard.disarm();
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
        // must return Ok. Go's `router.AppResponse`/`AppRequestFailed` treat
        // this as fatal (`ErrUnrequestedResponse`); this port deliberately
        // diverges and drops it silently instead (network.rs's
        // `handle_app_request_failed` doc has the full rationale).
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
    async fn app_request_rejects_id_still_pending() {
        let sender = Arc::new(RecordingSender::default());
        let network = P2pNetwork::new(test_node(0), sender.clone());
        let client = network
            .add_handler(TEST_HANDLER_ID, Arc::new(NoopHandler))
            .unwrap();
        let token = CancellationToken::new();
        let node = test_node(1);

        // A fresh network's request-id counter starts at 0
        // (`AtomicU32::new(0)`), so the very first `app_request` is
        // guaranteed to allocate id 0. Seed the pending map with that id
        // directly (the `client` test module is a descendant of `client`, so
        // `Client`'s private fields are visible here) to simulate "the
        // allocator handed out an id that's still outstanding" without
        // needing to actually wrap a `u32` counter.
        let stale_called = Arc::new(StdMutex::new(false));
        let stale_called_clone = stale_called.clone();
        client.pending.lock().insert(
            0,
            Box::new(move |_, _| {
                *stale_called_clone.lock().unwrap() = true;
            }),
        );

        let err = client
            .app_request(&token, node, b"req".to_vec(), Box::new(|_, _| {}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::Error::RequestPending(0)),
            "got: {err:?}"
        );

        // Nothing was ever sent for the rejected request...
        assert!(
            sender.requests.lock().unwrap().is_empty(),
            "a rejected app_request must not call send_app_request"
        );
        // ...and the original stale entry is untouched, not clobbered.
        assert!(
            client.pending.lock().contains_key(&0),
            "the original pending entry must survive the rejection"
        );
        network
            .handle_app_response(&token, node, 0, b"resp")
            .await
            .unwrap();
        assert!(
            *stale_called.lock().unwrap(),
            "the original (untouched) callback must still be resolvable"
        );
    }

    #[tokio::test]
    async fn app_request_cancelled_mid_send_does_not_leak_entry() {
        /// Signals `started` the instant `send_app_request` is entered, then
        /// hangs forever — so the caller can be certain the pending entry
        /// has already been inserted (this port inserts before the send,
        /// see the module doc) before cancelling.
        struct HangingSender {
            started: tokio::sync::Notify,
        }

        #[async_trait]
        impl AppSender for HangingSender {
            async fn send_app_request(
                &self,
                _token: &CancellationToken,
                _nodes: &HashSet<NodeId>,
                _request_id: u32,
                _bytes: Vec<u8>,
            ) -> VmResult<()> {
                self.started.notify_one();
                std::future::pending::<()>().await;
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

        let sender = Arc::new(HangingSender {
            started: tokio::sync::Notify::new(),
        });
        let network = P2pNetwork::new(test_node(0), sender.clone());
        let client = Arc::new(
            network
                .add_handler(TEST_HANDLER_ID, Arc::new(NoopHandler))
                .unwrap(),
        );
        let node = test_node(1);

        let called = Arc::new(StdMutex::new(false));
        let called_clone = called.clone();
        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            let token = CancellationToken::new();
            client_clone
                .app_request(
                    &token,
                    node,
                    b"req".to_vec(),
                    Box::new(move |_, _| {
                        *called_clone.lock().unwrap() = true;
                    }),
                )
                .await
        });

        // Block until the spawned task is provably suspended inside
        // `send_app_request` (i.e. past the pending-map insert), then abort
        // it — dropping its future, and with it the armed `PendingGuard`,
        // mid-`.await`.
        sender.started.notified().await;
        handle.abort();
        let result = handle.await;
        assert!(
            result.unwrap_err().is_cancelled(),
            "the spawned app_request task must have been cancelled, not completed"
        );

        // The pending entry must not have leaked past the cancellation.
        assert!(
            !client.pending.lock().contains_key(&0),
            "a cancelled app_request must not leak its pending entry"
        );

        // A later wire response for that id is dropped silently (same policy
        // as any other unknown/already-resolved id) rather than invoking the
        // orphaned callback.
        let token = CancellationToken::new();
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
