// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `PeerHandle` + `PeerCommand` — the external control surface for a running
//! [`crate::peer::peer::Peer`] actor (`specs/05` §3.2, `specs/17` §7.2).
//!
//! Go runs three goroutines sharing a `Peer` struct and exposes
//! `Send`/`StartSendGetPeerList`/`StartClose` plus the `onFinishHandshake` /
//! `onClosed` channels. In Rust those become a command channel
//! ([`PeerCommand`]) plus two latch [`CancellationToken`]s (level-triggered, so
//! awaiting after the event still returns), wrapped in a cheaply-cloneable
//! handle.

use ava_message::codec::OutboundMessage;
use ava_types::node_id::NodeId;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// A command sent to a peer actor (Go `Send` / `StartSendGetPeerList` /
/// `StartClose`).
#[derive(Debug)]
pub enum PeerCommand {
    /// Enqueue an outbound message on the peer's queue.
    Send(OutboundMessage),
    /// Coalesce a request to gossip our peer list to this peer (debounced via a
    /// capacity-1 channel; Go `getPeerListChan`).
    GetPeerList,
    /// Begin closing the peer (Go `StartClose`).
    Close,
}

/// The external control handle for a running peer actor. Cheap to clone; held
/// by the `Network` peer set.
#[derive(Clone)]
pub struct PeerHandle {
    /// The peer's NodeID.
    pub(crate) id: NodeId,
    /// Command channel into the peer's net-messages task.
    pub(crate) cmd_tx: mpsc::Sender<PeerCommand>,
    /// Latched once the application handshake completes (Go `onFinishHandshake`).
    pub(crate) finished_handshake: CancellationToken,
    /// Latched once all three peer tasks have drained (Go `onClosed`).
    pub(crate) closed: CancellationToken,
    /// The peer's cancellation token (a grandchild of the network token).
    pub(crate) close_token: CancellationToken,
}

impl PeerHandle {
    /// The peer's NodeID.
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.id
    }

    /// Enqueue `msg` for sending. Returns `false` if the actor has shut down.
    pub fn send(&self, msg: OutboundMessage) -> bool {
        self.cmd_tx.try_send(PeerCommand::Send(msg)).is_ok()
    }

    /// Request a peer-list gossip to this peer (coalesced).
    pub fn start_send_get_peer_list(&self) -> bool {
        self.cmd_tx.try_send(PeerCommand::GetPeerList).is_ok()
    }

    /// Begin closing the peer. Idempotent: cancels the peer token directly so
    /// shutdown proceeds even if the command channel is full.
    pub fn close(&self) {
        let _ = self.cmd_tx.try_send(PeerCommand::Close);
        self.close_token.cancel();
    }

    /// `true` once the application handshake has completed.
    #[must_use]
    pub fn has_finished_handshake(&self) -> bool {
        self.finished_handshake.is_cancelled()
    }

    /// Await the application handshake completing (returns immediately if it
    /// already has).
    pub async fn finished_handshake(&self) {
        self.finished_handshake.cancelled().await;
    }

    /// Await all peer tasks draining (returns immediately if already closed).
    pub async fn closed(&self) {
        self.closed.cancelled().await;
    }

    /// `true` if the peer has fully drained (non-blocking).
    #[must_use]
    pub fn closed_now(&self) -> bool {
        self.closed.is_cancelled()
    }
}
