// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Inbound handshake / ping / pong / peer-list handling for the [`Peer`] actor
//! (`specs/05` §1.4/§1.5, `specs/26` §3.1).
//!
//! `handle_handshake` validates every §1.4 disconnect reason and, on success,
//! replies with a `PeerList` (`bypass_throttling = true`). Receiving the peer's
//! `PeerList` while `got_handshake` finishes the handshake and notifies the
//! `Network` (`ExternalHandler::connected`). The full handshake validation and
//! gossip handling are layered in over M2.15–M2.17; this module starts with the
//! M2.14 scaffolding (set `got_handshake`, reply `PeerList`, handle ping/pong).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use ava_message::builder::OutboundMsgBuilder;
use ava_message::proto::p2p;

use crate::error::Error;
use crate::peer::peer::Peer;

impl Peer {
    /// Handle an inbound `Handshake` (`specs/05` §1.4). M2.14 scaffolding:
    /// reject a duplicate, set `got_handshake`, and reply with a `PeerList`. The
    /// full disconnect-reason validation lands in M2.15.
    pub(crate) fn handle_handshake(self: &Arc<Self>, _h: p2p::Handshake) -> crate::Result<()> {
        if self.got_handshake.swap(true, Ordering::AcqRel) {
            return Err(Error::DuplicateHandshake);
        }
        self.reply_peer_list()?;
        Ok(())
    }

    /// Handle an inbound `PeerList` (`specs/05` §1.4). If we have processed the
    /// peer's `Handshake` and the handshake is not yet finished, complete it.
    pub(crate) fn handle_peer_list(self: &Arc<Self>, _pl: p2p::PeerList) -> crate::Result<()> {
        if self.got_handshake.load(Ordering::Acquire) && !self.finished_handshake.is_cancelled() {
            self.finish_handshake();
        }
        Ok(())
    }

    /// Handle an inbound `GetPeerList` (`specs/05` §1.4). Not answered until the
    /// handshake has finished. Full gossip response is M2.17.
    pub(crate) fn handle_get_peer_list(
        self: &Arc<Self>,
        _gpl: p2p::GetPeerList,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Handle an inbound `Ping` (`specs/05` §1.5): store the peer's claimed
    /// uptime (reject if `> 100`) and reply with a `Pong`.
    pub(crate) fn handle_ping(self: &Arc<Self>, ping: p2p::Ping) -> crate::Result<()> {
        if ping.uptime > 100 {
            return Err(Error::InvalidUptime(ping.uptime));
        }
        self.observed_uptime.store(ping.uptime, Ordering::Relaxed);
        let pong = self.cfg.creator.pong()?;
        self.reply(pong);
        Ok(())
    }

    /// Handle an inbound `Pong` (`specs/05` §1.5): an unsolicited `Pong` (no
    /// `Ping` outstanding) closes the connection; otherwise clear the
    /// outstanding-ping marker.
    pub(crate) fn handle_pong(self: &Arc<Self>, _pong: p2p::Pong) -> crate::Result<()> {
        let last = self.last_ping_sent_nanos.swap(0, Ordering::AcqRel);
        if last == 0 {
            return Err(Error::UnsolicitedPong);
        }
        Ok(())
    }

    /// Replies with a `PeerList` to the peer's `Handshake` (`bypass_throttling`).
    /// Full IP-gossip content lands in M2.17; M2.14 sends an empty list.
    fn reply_peer_list(self: &Arc<Self>) -> crate::Result<()> {
        let peer_list = self.cfg.creator.peer_list(&[], true)?;
        self.reply(peer_list);
        Ok(())
    }

    /// Enqueue a reply on the outbound queue.
    fn reply(self: &Arc<Self>, msg: ava_message::codec::OutboundMessage) {
        use crate::peer::message_queue::MessageQueue;
        self.queue.push(msg);
    }

    /// Finalize the handshake: latch `finished_handshake` and notify the router
    /// (`ExternalHandler::connected`) for each shared subnet (`specs/05` §3.7).
    fn finish_handshake(self: &Arc<Self>) {
        self.finished_handshake.cancel();

        let version = self
            .hs
            .lock()
            .version
            .clone()
            .unwrap_or_else(|| self.cfg.my_version.clone());

        // Notify for the intersection of our and the peer's tracked subnets,
        // always including the primary network. M2.17 refines the subnet set;
        // M2.14/M2.15 notify on the primary network.
        self.cfg
            .router
            .connected(self.id, &version, ava_types::id::Id::default());
    }
}
