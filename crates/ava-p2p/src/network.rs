// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The varint-prefixed protocol mux (Go `network/p2p/network.go` `Network` +
//! `network/p2p/router.go` `router`).
//!
//! [`P2pNetwork`] dispatches inbound `AppGossip`/`AppRequest` payloads to
//! whichever [`Handler`] was registered under the payload's varint-encoded
//! prefix (Go `ProtocolPrefix`/`ParseMessage`), mirroring the Go `router`'s
//! `parse` + `responder` dispatch (`network/p2p/router.go:107-138,261`).
//!
//! ## `&self` vs. `&mut self`
//!
//! `ava_vm::{AppHandler, Connector}` take `&mut self`, but production callers
//! hold `P2pNetwork` behind a shared `Arc<P2pNetwork>` (it's also handed out
//! to every [`Handler`]/future `Client` for the lifetime of the node) — a
//! shared `Arc` can never yield the `&mut self` those trait methods need
//! (`Arc::get_mut` only succeeds with a unique refcount, which doesn't hold
//! once any clone is outstanding; `unsafe` aliasing is forbidden crate-wide).
//! `crates/ava-avm/src/network/atomic.rs` hit the same wall and worked around
//! it with a bespoke local `&self` trait; `P2pNetwork` instead exposes its
//! *entire* dispatch surface as inherent `&self` methods
//! (`handle_app_request`/`handle_app_response`/`handle_app_request_failed`/
//! `handle_app_gossip`/`handle_connected`/`handle_disconnected`, all sound
//! because the mutable state they touch already lives behind
//! `parking_lot::Mutex`), and the `AppHandler`/`Connector` trait impls below
//! are one-line `&mut self` delegations to those methods. Callers that need
//! the trait objects (e.g. a uniquely-owned `Box<dyn AppHandler>`) still work;
//! callers holding a shared `Arc<P2pNetwork>` (e.g. Task 12's `EvmVm`
//! delegation) call the inherent `&self` methods directly instead.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use ava_types::node_id::NodeId;
use ava_version::application::Application;
use ava_vm::app::{AppError, AppHandler};
use ava_vm::app_sender::AppSender;
use ava_vm::connector::Connector;
use ava_vm::error::Result as VmResult;

use crate::client::{Client, PendingMap};
use crate::handler::{Handler, err_unregistered_handler};

/// Encode `handler_id` as an unsigned LEB128 varint (Go `binary.AppendUvarint`,
/// `network/p2p/network.go` `ProtocolPrefix`).
#[must_use]
pub fn protocol_prefix(handler_id: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    prost::encoding::encode_varint(handler_id, &mut buf);
    buf
}

/// Parse a varint-prefixed message into `(handler_id, remaining payload)`
/// (Go `network/p2p/router.go` `ParseMessage`). Returns `None` if `msg` is
/// empty or does not start with a valid varint.
#[must_use]
pub fn parse_prefix(msg: &[u8]) -> Option<(u64, &[u8])> {
    let mut rest = msg;
    let handler_id = prost::encoding::decode_varint(&mut rest).ok()?;
    Some((handler_id, rest))
}

/// Exposes networking state and dispatches p2p application protocols
/// (Go `network/p2p/network.go` `Network`). See the module doc for why this
/// type's dispatch surface is inherent `&self` methods rather than living
/// only in the `AppHandler`/`Connector` trait impls.
pub struct P2pNetwork {
    /// This node's own id (currently unused by dispatch; kept for parity with
    /// the constructor shape and for handlers that need it later).
    node_id: NodeId,
    sender: Arc<dyn AppSender>,
    handlers: Mutex<HashMap<u64, Arc<dyn Handler>>>,
    peers: Mutex<BTreeSet<NodeId>>,
    /// Seeds the deterministic LCG in [`Self::sample_peer`].
    sample_counter: AtomicU64,
    /// In-flight `AppRequest`s awaiting a response/failure, shared with every
    /// [`Client`] this network hands out (Go `router.pendingAppRequests`).
    pending: PendingMap,
    /// Allocates request ids for every [`Client`] issued off this network —
    /// one shared id space, like Go's `router.requestID`. See `client.rs`'s
    /// module doc for why this port doesn't reserve odd numbers the way Go
    /// does.
    request_id: Arc<AtomicU32>,
}

impl P2pNetwork {
    /// Constructs a new `P2pNetwork` bound to `node_id`, sending outbound
    /// application messages via `sender` (Go `NewNetwork`; the metrics
    /// registerer and `ConnectionHandler` list are elided — this port has no
    /// Prometheus wiring yet and `Connector` is implemented directly).
    #[must_use]
    pub fn new(node_id: NodeId, sender: Arc<dyn AppSender>) -> Arc<Self> {
        Arc::new(Self {
            node_id,
            sender,
            handlers: Mutex::new(HashMap::new()),
            peers: Mutex::new(BTreeSet::new()),
            sample_counter: AtomicU64::new(0),
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_id: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Returns this node's own id, as passed to [`P2pNetwork::new`].
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Reserves `handler_id` for `handler` and returns a [`Client`] for
    /// issuing correlated requests/gossip under that handler id (Go
    /// `Network.AddHandler` / `router.addHandler`,
    /// `network/p2p/router.go:88-104`, plus `Network.NewClient`). Returns
    /// [`crate::Error::DuplicateHandler`] if `handler_id` is already
    /// registered, mirroring Go's `ErrExistingAppProtocol`.
    ///
    /// The returned `Client` shares this network's pending-request map and
    /// request-id counter, so a response/failure this network dispatches via
    /// [`Self::handle_app_response`]/[`Self::handle_app_request_failed`]
    /// resolves the callback the `Client` registered.
    pub fn add_handler(&self, handler_id: u64, handler: Arc<dyn Handler>) -> crate::Result<Client> {
        let mut handlers = self.handlers.lock();
        if handlers.contains_key(&handler_id) {
            return Err(crate::Error::DuplicateHandler(handler_id));
        }
        handlers.insert(handler_id, handler);
        Ok(Client::new(
            handler_id,
            self.sender.clone(),
            self.pending.clone(),
            self.request_id.clone(),
        ))
    }

    /// Uniformly samples one connected peer, or `None` if there are none
    /// (Go `PeerSampler.Sample` with `limit == 1`, backed by
    /// `set.SampleableSet`'s Fisher-Yates-style shuffle).
    ///
    /// This port has no RNG-crate dependency, so it substitutes a small
    /// counter-seeded linear congruential generator (Numerical Recipes
    /// constants) rather than Go's sampler: deterministic given the call
    /// count, and adequate because callers only need *some* connected peer,
    /// not an adversarially-unpredictable one. This diverges from Go's
    /// sampler, which is intended to be unpredictable across calls.
    #[must_use]
    pub fn sample_peer(&self) -> Option<NodeId> {
        let peers = self.peers.lock();
        let len = peers.len();
        if len == 0 {
            return None;
        }
        let counter = self.sample_counter.fetch_add(1, Ordering::Relaxed);
        let x = counter
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // `len > 0` is checked above, so `checked_rem` never actually hits its
        // `None` (divide-by-zero) arm; using it (rather than `%`/`wrapping_rem`,
        // both of which panic on a zero divisor) keeps this arithmetic_side_effects-clean.
        let idx = x.checked_rem(len as u64).unwrap_or(0) as usize;
        peers.iter().nth(idx).copied()
    }

    /// `&self` dispatch for an inbound `AppRequest` (Go `router.AppRequest`):
    /// parses the varint handler-id prefix, and either forwards the
    /// unprefixed payload to the registered [`Handler`] (sending its response
    /// or `AppError` back via `sender`) or, if the prefix is unparsable or
    /// unregistered, replies with `ErrUnregisteredHandler`.
    pub async fn handle_app_request(
        &self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> VmResult<()> {
        let parsed = parse_prefix(request).and_then(|(handler_id, msg)| {
            self.handlers
                .lock()
                .get(&handler_id)
                .cloned()
                .map(|handler| (handler, msg))
        });
        let Some((handler, msg)) = parsed else {
            tracing::debug!(
                node = %node,
                request_id,
                "received app request for unregistered handler"
            );
            let err = err_unregistered_handler();
            return self
                .sender
                .send_app_error(token, node, request_id, err.code, &err.message)
                .await;
        };

        match handler.app_request(node, deadline, msg).await {
            Ok(response) => {
                self.sender
                    .send_app_response(token, node, request_id, response)
                    .await
            }
            Err(err) => {
                self.sender
                    .send_app_error(token, node, request_id, err.code, &err.message)
                    .await
            }
        }
    }

    /// `&self` dispatch for an inbound `AppRequestFailed` (Go
    /// `router.AppRequestFailed`): removes the pending entry for
    /// `request_id`, if any, and invokes its callback with `Err(err)` exactly
    /// once. A failure for an unknown/already-resolved id is dropped silently
    /// — the engine router's timeout synthesis can race a real reply, so
    /// dedup-by-removal (rather than treating this as an error) is the same
    /// safety the Go router relies on.
    pub async fn handle_app_request_failed(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        let callback = self.pending.lock().remove(&request_id);
        let Some(callback) = callback else {
            tracing::debug!(
                node = %node,
                request_id,
                code = err.code,
                "app request failed for unknown/already-resolved request id"
            );
            return Ok(());
        };
        callback(node, Err(err));
        Ok(())
    }

    /// `&self` dispatch for an inbound `AppResponse` (Go `router.AppResponse`):
    /// removes the pending entry for `request_id`, if any, and invokes its
    /// callback with `Ok(response)` exactly once. A response for an
    /// unknown/already-resolved id is dropped silently — see
    /// [`Self::handle_app_request_failed`]'s doc for why.
    pub async fn handle_app_response(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        let callback = self.pending.lock().remove(&request_id);
        let Some(callback) = callback else {
            tracing::debug!(
                node = %node,
                request_id,
                "app response received for unknown/already-resolved request id"
            );
            return Ok(());
        };
        callback(node, Ok(response.to_vec()));
        Ok(())
    }

    /// `&self` dispatch for an inbound `AppGossip` (Go `router.AppGossip`):
    /// parses the varint handler-id prefix and forwards the unprefixed
    /// payload to the registered [`Handler`], or silently drops it if the
    /// prefix is unparsable or unregistered.
    pub async fn handle_app_gossip(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        let parsed = parse_prefix(msg).and_then(|(handler_id, payload)| {
            self.handlers
                .lock()
                .get(&handler_id)
                .cloned()
                .map(|handler| (handler, payload))
        });
        let Some((handler, payload)) = parsed else {
            tracing::debug!(node = %node, "received app gossip for unregistered handler");
            return Ok(());
        };

        handler.app_gossip(node, payload).await;
        Ok(())
    }

    /// `&self` dispatch for a peer connecting (Go `Network.Connected`).
    pub async fn handle_connected(
        &self,
        _token: &CancellationToken,
        node: NodeId,
        _version: Application,
    ) -> VmResult<()> {
        self.peers.lock().insert(node);
        Ok(())
    }

    /// `&self` dispatch for a peer disconnecting (Go `Network.Disconnected`).
    pub async fn handle_disconnected(
        &self,
        _token: &CancellationToken,
        node: NodeId,
    ) -> VmResult<()> {
        self.peers.lock().remove(&node);
        Ok(())
    }
}

#[async_trait]
impl AppHandler for P2pNetwork {
    async fn app_request(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        deadline: Instant,
        request: &[u8],
    ) -> VmResult<()> {
        self.handle_app_request(token, node, request_id, deadline, request)
            .await
    }

    async fn app_request_failed(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        err: AppError,
    ) -> VmResult<()> {
        self.handle_app_request_failed(token, node, request_id, err)
            .await
    }

    async fn app_response(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        request_id: u32,
        response: &[u8],
    ) -> VmResult<()> {
        self.handle_app_response(token, node, request_id, response)
            .await
    }

    async fn app_gossip(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        msg: &[u8],
    ) -> VmResult<()> {
        self.handle_app_gossip(token, node, msg).await
    }
}

#[async_trait]
impl Connector for P2pNetwork {
    async fn connected(
        &mut self,
        token: &CancellationToken,
        node: NodeId,
        version: Application,
    ) -> VmResult<()> {
        self.handle_connected(token, node, version).await
    }

    async fn disconnected(&mut self, token: &CancellationToken, node: NodeId) -> VmResult<()> {
        self.handle_disconnected(token, node).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use ava_vm::app_sender::SendConfig;

    use super::*;

    #[test]
    fn protocol_prefix_matches_go_append_uvarint() {
        assert_eq!(protocol_prefix(0), vec![0x00]);
        assert_eq!(protocol_prefix(1), vec![0x01]);
        assert_eq!(protocol_prefix(127), vec![0x7f]);
        assert_eq!(protocol_prefix(128), vec![0x80, 0x01]);
    }

    #[test]
    fn parse_prefix_splits_handler_id_and_payload() {
        let mut framed = protocol_prefix(1);
        framed.extend_from_slice(b"payload");
        let (id, rest) = parse_prefix(&framed).unwrap();
        assert_eq!(id, 1);
        assert_eq!(rest, b"payload");
        assert!(parse_prefix(&[]).is_none());
    }

    /// Records every call made through it; used in place of a mock crate
    /// since `ava-vm` isn't yet on `mockall`.
    #[derive(Default)]
    struct RecordingSender {
        responses: StdMutex<Vec<(NodeId, u32, Vec<u8>)>>,
        errors: StdMutex<Vec<(NodeId, u32, i32, String)>>,
    }

    #[async_trait]
    impl AppSender for RecordingSender {
        async fn send_app_request(
            &self,
            _token: &CancellationToken,
            _nodes: &std::collections::HashSet<NodeId>,
            _request_id: u32,
            _bytes: Vec<u8>,
        ) -> VmResult<()> {
            Ok(())
        }

        async fn send_app_response(
            &self,
            _token: &CancellationToken,
            node: NodeId,
            request_id: u32,
            bytes: Vec<u8>,
        ) -> VmResult<()> {
            self.responses
                .lock()
                .unwrap()
                .push((node, request_id, bytes));
            Ok(())
        }

        async fn send_app_error(
            &self,
            _token: &CancellationToken,
            node: NodeId,
            request_id: u32,
            code: i32,
            message: &str,
        ) -> VmResult<()> {
            self.errors
                .lock()
                .unwrap()
                .push((node, request_id, code, message.to_string()));
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

    /// Records every `app_gossip`/`app_request` call it receives.
    #[derive(Default)]
    struct RecordingHandler {
        gossips: StdMutex<Vec<(NodeId, Vec<u8>)>>,
    }

    #[async_trait]
    impl Handler for RecordingHandler {
        async fn app_gossip(&self, node: NodeId, msg: &[u8]) {
            self.gossips.lock().unwrap().push((node, msg.to_vec()));
        }

        async fn app_request(
            &self,
            _node: NodeId,
            _deadline: Instant,
            msg: &[u8],
        ) -> Result<Vec<u8>, AppError> {
            Ok(msg.to_vec())
        }
    }

    fn test_node(byte: u8) -> NodeId {
        NodeId::from([byte; 20])
    }

    /// Builds a `P2pNetwork` the same way production callers do — through
    /// `P2pNetwork::new`'s `Arc<Self>` — and drives it via the inherent
    /// `&self` `handle_*` methods, proving the production (shared-`Arc`)
    /// shape actually works end to end.
    fn test_network(sender: Arc<dyn AppSender>) -> Arc<P2pNetwork> {
        P2pNetwork::new(test_node(0), sender)
    }

    #[tokio::test]
    async fn dispatches_gossip_to_registered_handler() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender);
        let handler = Arc::new(RecordingHandler::default());
        network.add_handler(0, handler.clone()).unwrap();

        let token = CancellationToken::new();
        let node = test_node(1);
        let mut framed = protocol_prefix(0);
        framed.extend_from_slice(b"hello");

        network
            .handle_app_gossip(&token, node, &framed)
            .await
            .unwrap();

        let gossips = handler.gossips.lock().unwrap();
        assert_eq!(gossips.len(), 1);
        let (got_node, got_msg) = gossips.first().unwrap();
        assert_eq!(*got_node, node);
        assert_eq!(got_msg, b"hello");
    }

    #[tokio::test]
    async fn drops_gossip_for_unregistered_handler() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender);
        let token = CancellationToken::new();
        let node = test_node(1);
        let mut framed = protocol_prefix(99);
        framed.extend_from_slice(b"hello");

        // Unregistered-handler gossip is silently dropped (Go
        // `router.AppGossip`'s `!ok` branch): no panic, no error, no send.
        network
            .handle_app_gossip(&token, node, &framed)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn request_dispatches_and_sends_response() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender.clone());
        let handler = Arc::new(RecordingHandler::default());
        network.add_handler(0, handler).unwrap();

        let token = CancellationToken::new();
        let node = test_node(1);
        let mut framed = protocol_prefix(0);
        framed.extend_from_slice(b"req");

        network
            .handle_app_request(&token, node, 7, Instant::now(), &framed)
            .await
            .unwrap();

        let responses = sender.responses.lock().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses.first().unwrap(), &(node, 7, b"req".to_vec()));
    }

    #[tokio::test]
    async fn request_for_unregistered_handler_sends_app_error() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender.clone());
        let token = CancellationToken::new();
        let node = test_node(1);
        let framed = protocol_prefix(99);

        network
            .handle_app_request(&token, node, 7, Instant::now(), &framed)
            .await
            .unwrap();

        let errors = sender.errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        let (got_node, got_request_id, got_code, got_message) = errors.first().unwrap();
        assert_eq!(*got_node, node);
        assert_eq!(*got_request_id, 7);
        assert_eq!(*got_code, -2);
        assert_eq!(got_message, "unregistered handler");
    }

    #[tokio::test]
    async fn connect_and_disconnect_update_peers_and_sample() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender);
        assert_eq!(network.sample_peer(), None);

        let token = CancellationToken::new();
        let node = test_node(1);
        network
            .handle_connected(&token, node, Application::new("avalanchers", 1, 0, 0))
            .await
            .unwrap();

        assert_eq!(network.sample_peer(), Some(node));

        network.handle_disconnected(&token, node).await.unwrap();
        assert_eq!(network.sample_peer(), None);
    }

    #[test]
    fn add_handler_rejects_duplicate_id() {
        let sender = Arc::new(RecordingSender::default());
        let network = test_network(sender);
        let handler_a = Arc::new(RecordingHandler::default());
        let handler_b = Arc::new(RecordingHandler::default());

        network.add_handler(0, handler_a).unwrap();
        let err = network.add_handler(0, handler_b).unwrap_err();
        assert!(matches!(err, crate::Error::DuplicateHandler(0)));
    }

    #[tokio::test]
    async fn app_handler_trait_impl_delegates_through_mut_self() {
        // Proves the `&mut self` `AppHandler`/`Connector` impls still work
        // for uniquely-owned callers (not just the inherent `&self` surface
        // exercised by the tests above).
        let sender = Arc::new(RecordingSender::default());
        let mut network = P2pNetwork {
            node_id: test_node(0),
            sender,
            handlers: Mutex::new(HashMap::new()),
            peers: Mutex::new(BTreeSet::new()),
            sample_counter: AtomicU64::new(0),
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_id: Arc::new(AtomicU32::new(0)),
        };
        let handler = Arc::new(RecordingHandler::default());
        network.add_handler(0, handler.clone()).unwrap();

        let token = CancellationToken::new();
        let node = test_node(1);
        let mut framed = protocol_prefix(0);
        framed.extend_from_slice(b"hello");

        AppHandler::app_gossip(&mut network, &token, node, &framed)
            .await
            .unwrap();

        assert_eq!(handler.gossips.lock().unwrap().len(), 1);
    }
}
