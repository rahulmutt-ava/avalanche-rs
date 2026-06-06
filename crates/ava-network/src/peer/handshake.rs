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
use crate::peer::ip::SignedIp;
use crate::peer::peer::Peer;

impl Peer {
    /// Handle an inbound `Handshake` (`specs/05` §1.4): validate every
    /// disconnect reason, record the peer's handshake state, then reply with a
    /// `PeerList` (`bypass_throttling = true`). Returns `Err` (→ close) on any
    /// violation.
    pub(crate) fn handle_handshake(self: &Arc<Self>, h: p2p::Handshake) -> crate::Result<()> {
        // Reject a duplicate Handshake first (Go: a second Handshake is fatal).
        if self.got_handshake.swap(true, Ordering::AcqRel) {
            return Err(Error::DuplicateHandshake);
        }

        // 1. network_id match.
        if h.network_id != self.cfg.network_id {
            return Err(Error::NetworkIdMismatch {
                peer: h.network_id,
                ours: self.cfg.network_id,
            });
        }

        // 2. clock skew ≤ maxClockDifference.
        let our_time = self.cfg.clock.unix();
        let skew = our_time.abs_diff(h.my_time);
        if skew > crate::config::MAX_CLOCK_DIFFERENCE.as_secs() {
            return Err(Error::ClockSkew {
                peer: h.my_time,
                ours: our_time,
            });
        }

        // 3. parse Client → version; check compatibility (`specs/26` §3.1).
        let client = h
            .client
            .ok_or_else(|| Error::MalformedHandshake("missing client".into()))?;
        let version =
            ava_version::Application::new(client.name, client.major, client.minor, client.patch);
        if !self.is_compatible(&version) {
            return Err(Error::IncompatibleVersion(version.display()));
        }

        // 4. ≤ maxNumTrackedSubnets tracked subnets.
        if h.tracked_subnets.len() > crate::config::MAX_NUM_TRACKED_SUBNETS {
            return Err(Error::TooManyTrackedSubnets(h.tracked_subnets.len()));
        }
        let mut tracked_subnets = Vec::with_capacity(h.tracked_subnets.len());
        for raw in &h.tracked_subnets {
            let id = ava_types::id::Id::from_slice(raw)
                .map_err(|e| Error::MalformedHandshake(format!("tracked subnet: {e}")))?;
            tracked_subnets.push(id);
        }

        // 5. supported ∩ objected == ∅.
        let supported: std::collections::BTreeSet<u32> = h.supported_acps.iter().copied().collect();
        if h.objected_acps.iter().any(|a| supported.contains(a)) {
            return Err(Error::AcpConflict);
        }

        // 6. valid IP / non-zero port.
        let port = u16::try_from(h.ip_port).map_err(|_| Error::InvalidPeerIp)?;
        if port == 0 {
            return Err(Error::InvalidPeerIp);
        }
        let ip = ip_from_bytes(&h.ip_addr).ok_or(Error::InvalidPeerIp)?;

        // 9. bloom salt ≤ maxBloomSaltLen.
        if let Some(bf) = &h.known_peers
            && bf.salt.len() > crate::config::MAX_BLOOM_SALT_LEN
        {
            return Err(Error::BloomSaltTooLong(bf.salt.len()));
        }

        // 7. verify the signed IP (TLS sig over ip||port||ts) against the peer
        //    cert. max_timestamp = now + 60s (`specs/05` §1.6).
        let signed = SignedIp {
            unsigned: crate::peer::ip::UnsignedIp::new(ip, port, h.ip_signing_time),
            tls_signature: h.ip_node_id_sig.to_vec(),
            bls_signature_bytes: h.ip_bls_sig.to_vec(),
        };
        let max_ts = our_time.saturating_add(crate::config::MAX_CLOCK_DIFFERENCE.as_secs());
        signed.verify(&self.cert, max_ts)?;

        // Record the peer's handshake state.
        {
            let mut hs = self.hs.lock();
            hs.ip = Some(signed);
            hs.version = Some(version);
            hs.tracked_subnets = tracked_subnets;
        }

        // Reply with our PeerList (completes the peer's half of the handshake).
        self.reply_peer_list()?;
        Ok(())
    }

    /// Handle an inbound `PeerList` (`specs/05` §1.4/§3.5). Authenticate and
    /// track each `ClaimedIpPort` (a bad signed IP is dropped, not fatal — Go
    /// logs and skips). If we have processed the peer's `Handshake` and the
    /// handshake is not yet finished, complete it.
    pub(crate) fn handle_peer_list(self: &Arc<Self>, pl: p2p::PeerList) -> crate::Result<()> {
        let now = self.cfg.clock.unix();
        for claimed in &pl.claimed_ip_ports {
            // Track only verified claims; ignore (don't disconnect on) a bad one.
            let _ = self.cfg.ip_tracker.add_claimed_ip_port(claimed, now);
        }

        if self.got_handshake.load(Ordering::Acquire) && !self.finished_handshake.is_cancelled() {
            self.finish_handshake();
        }
        Ok(())
    }

    /// Handle an inbound `GetPeerList` (`specs/05` §1.4/§3.5). Not answered
    /// until the handshake has finished; then reply with the validator IPs the
    /// requester does not yet know (per its bloom filter + salt).
    pub(crate) fn handle_get_peer_list(
        self: &Arc<Self>,
        gpl: p2p::GetPeerList,
    ) -> crate::Result<()> {
        // GetPeerList is not answered until the handshake is finished.
        if !self.finished_handshake.is_cancelled() {
            return Ok(());
        }
        let (filter, salt) = match &gpl.known_peers {
            Some(bf) => (bf.filter.clone(), bf.salt.clone()),
            None => return Ok(()),
        };
        // A salt over the max is a protocol error (cross-check §1.4).
        let peers = self.cfg.ip_tracker.peers(&filter, &salt)?;
        if !peers.is_empty() {
            let msg = self.cfg.creator.peer_list(&peers, false)?;
            self.reply(msg);
        }
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

    /// Re-applies the `version.Compatibility` floor rule using this peer's
    /// **injected clock** rather than the wall clock, so the fork-boundary
    /// cut-over is testable (`specs/26` §3.1). Reads the (public) fields of the
    /// shared `Compatibility`.
    pub(crate) fn is_compatible(self: &Arc<Self>, peer: &ava_version::Application) -> bool {
        let compat = &self.cfg.version_compatibility;
        // Clause 1: reject a peer on a newer major than us.
        if compat.current.major < peer.major {
            return false;
        }
        // Clause 2: select the floor by the injected clock vs upgrade_time.
        let floor = if self.cfg.clock.now_system() < compat.upgrade_time {
            &compat.min_compatible
        } else {
            &compat.min_compatible_after_upgrade
        };
        peer >= floor
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

/// Decode a handshake `ip_addr` (16-byte `As16` form, or a bare 4-byte IPv4 if a
/// legacy peer sends one) into an [`std::net::IpAddr`]. An IPv4-mapped IPv6
/// address is unmapped back to IPv4. Returns `None` on an unusable length or the
/// unspecified address.
fn ip_from_bytes(b: &[u8]) -> Option<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match b.len() {
        4 => {
            let octets: [u8; 4] = b.try_into().ok()?;
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        16 => {
            let octets: [u8; 16] = b.try_into().ok()?;
            let v6 = Ipv6Addr::from(octets);
            // Unmap an IPv4-mapped IPv6 address to its IPv4 form.
            match v6.to_ipv4_mapped() {
                Some(v4) => IpAddr::V4(v4),
                None => IpAddr::V6(v6),
            }
        }
        _ => return None,
    };
    if ip.is_unspecified() {
        return None;
    }
    Some(ip)
}
