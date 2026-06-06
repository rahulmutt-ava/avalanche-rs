// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `IpTracker` — the peer-list-gossip / IP-tracking state (`specs/05` §3.5).
//!
//! Mirrors Go `network/ip_tracker.go`. The tracker holds the most-recent
//! verified `(ip, port, timestamp)` claim for each node we have learned about,
//! and answers `GetPeerList` requests by returning the claims a requester does
//! **not** already know (per its supplied bloom filter + salt), so we never
//! re-send a peer the requester already has.
//!
//! On an inbound `PeerList`, each `ClaimedIpPort` is authenticated before it is
//! tracked: the embedded X.509 cert is strict-parsed, the signed IP is verified
//! against it, and only a valid claim is recorded (`specs/05` §3.5).
//!
//! Cadence constants (gossip 1m, pull 2s, bloom reset 1m, ≤15 validator IPs)
//! live here for the `runTimers` loop (M2.18) to consume.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use ava_message::proto::p2p;
use ava_types::node_id::NodeId;
use parking_lot::Mutex;

use crate::config::MAX_BLOOM_SALT_LEN;
use crate::error::{Error, Result};
use crate::network::bloom::ReadFilter;
use crate::network::tracked_ip::ClaimedIp;
use crate::peer::ip::{SignedIp, UnsignedIp};

/// Push-gossip period (Go `DefaultNetworkPeerListGossipFreq`).
pub const PEER_LIST_GOSSIP_FREQ: std::time::Duration = std::time::Duration::from_secs(60);
/// Pull-gossip (`GetPeerList`) period (Go `DefaultNetworkPeerListPullGossipFreq`).
pub const PEER_LIST_PULL_GOSSIP_FREQ: std::time::Duration = std::time::Duration::from_secs(2);
/// Bloom-filter reset period (Go `DefaultNetworkPeerListBloomResetFreq`).
pub const PEER_LIST_BLOOM_RESET_FREQ: std::time::Duration = std::time::Duration::from_secs(60);
/// Max validator IPs gossiped per message (Go `DefaultNetworkPeerListNumValidatorIPs`).
pub const PEER_LIST_NUM_VALIDATOR_IPS: usize = 15;

/// Tracks learned validator IPs + answers peer-list-gossip queries.
#[derive(Default)]
pub struct IpTracker {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// node -> its most-recent verified claim (sorted for determinism).
    claims: BTreeMap<NodeId, ClaimedIp>,
}

impl IpTracker {
    /// A fresh, empty tracker.
    #[must_use]
    pub fn new() -> IpTracker {
        IpTracker::default()
    }

    /// Number of currently-tracked claims.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().claims.len()
    }

    /// Whether the tracker holds no claims.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().claims.is_empty()
    }

    /// Whether we hold a claim for `node`.
    #[must_use]
    pub fn contains(&self, node: &NodeId) -> bool {
        self.inner.lock().claims.contains_key(node)
    }

    /// `ManuallyTrack` — record a node's address directly (no signature; used
    /// for bootstrappers / `manually_track`). The claim is keyed by `node`.
    pub fn manually_track(&self, node: NodeId, addr: SocketAddr) {
        let claim = ClaimedIp {
            addr,
            timestamp: 0,
            tls_signature: Vec::new(),
            cert_der: Vec::new(),
            tx_id: ava_types::id::Id::default(),
        };
        self.inner.lock().claims.insert(node, claim);
    }

    /// Authenticate and track an inbound `ClaimedIpPort` (Go `AddIP`).
    ///
    /// Strict-parses the embedded cert, verifies the signed IP against it
    /// (`max_timestamp = now + 60s`), and records the claim only if valid.
    /// Returns the verified peer's NodeID.
    ///
    /// # Errors
    /// - [`Error::InvalidPeerIp`] for a zero port / unusable IP.
    /// - the signed-IP verification errors ([`Error::InvalidTlsSignature`] /
    ///   [`Error::TimestampTooFarInFuture`]).
    /// - [`Error::CertificateParse`] if the cert fails the strict parser.
    pub fn add_claimed_ip_port(
        &self,
        claimed: &p2p::ClaimedIpPort,
        now_unix: u64,
    ) -> Result<NodeId> {
        let cert = ava_crypto::staking::parse_certificate(&claimed.x509_certificate)?;
        let node_id = crate::peer::upgrader::node_id_from_cert(&cert);

        let port = u16::try_from(claimed.ip_port).map_err(|_| Error::InvalidPeerIp)?;
        if port == 0 {
            return Err(Error::InvalidPeerIp);
        }
        let ip = ip_from_bytes(&claimed.ip_addr).ok_or(Error::InvalidPeerIp)?;

        let signed = SignedIp {
            unsigned: UnsignedIp::new(ip, port, claimed.timestamp),
            tls_signature: claimed.signature.to_vec(),
            bls_signature_bytes: Vec::new(),
        };
        let max_ts = now_unix.saturating_add(crate::config::MAX_CLOCK_DIFFERENCE.as_secs());
        signed.verify(&cert, max_ts)?;

        let tx_id =
            ava_types::id::Id::from_slice(&claimed.tx_id).unwrap_or_else(|_| ava_types::id::Id::default());

        let claim = ClaimedIp {
            addr: SocketAddr::new(ip, port),
            timestamp: claimed.timestamp,
            tls_signature: claimed.signature.to_vec(),
            cert_der: claimed.x509_certificate.to_vec(),
            tx_id,
        };

        // Only keep the most-recent claim for a node.
        let mut inner = self.inner.lock();
        let replace = inner
            .claims
            .get(&node_id)
            .is_none_or(|existing| claimed.timestamp >= existing.timestamp);
        if replace {
            inner.claims.insert(node_id, claim);
        }
        Ok(node_id)
    }

    /// `Peers` — return the tracked claims the requester does **not** already
    /// know, per its bloom filter + salt (Go `ip_tracker.GetGossipableIPs`).
    /// Returns at most [`PEER_LIST_NUM_VALIDATOR_IPS`] claims.
    ///
    /// # Errors
    /// [`Error::BloomSaltTooLong`] if `salt` exceeds `maxBloomSaltLen` (32);
    /// any bloom-parse error is surfaced as [`Error::MalformedHandshake`].
    pub fn peers(
        &self,
        filter_bytes: &[u8],
        salt: &[u8],
    ) -> Result<Vec<p2p::ClaimedIpPort>> {
        if salt.len() > MAX_BLOOM_SALT_LEN {
            return Err(Error::BloomSaltTooLong(salt.len()));
        }
        let filter = ReadFilter::parse(filter_bytes)
            .map_err(|e| Error::MalformedHandshake(format!("bloom filter: {e}")))?;

        let inner = self.inner.lock();
        let mut out = Vec::new();
        for (node, claim) in &inner.claims {
            if out.len() >= PEER_LIST_NUM_VALIDATOR_IPS {
                break;
            }
            // Skip an IP the requester already knows (its node id is in the
            // bloom filter, salted).
            if filter.contains_key(node.as_bytes(), salt) {
                continue;
            }
            out.push(p2p::ClaimedIpPort {
                x509_certificate: bytes::Bytes::copy_from_slice(&claim.cert_der),
                ip_addr: bytes::Bytes::copy_from_slice(&addr_as16(claim.addr.ip())),
                ip_port: u32::from(claim.addr.port()),
                timestamp: claim.timestamp,
                signature: bytes::Bytes::copy_from_slice(&claim.tls_signature),
                tx_id: bytes::Bytes::copy_from_slice(claim.tx_id.as_bytes()),
            });
        }
        Ok(out)
    }
}

/// The 16-byte `As16` form of an IP address (IPv4 → IPv4-mapped IPv6).
fn addr_as16(ip: std::net::IpAddr) -> [u8; 16] {
    match ip {
        std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        std::net::IpAddr::V6(v6) => v6.octets(),
    }
}

/// Decode a 4- or 16-byte address blob, unmapping IPv4-mapped IPv6.
fn ip_from_bytes(b: &[u8]) -> Option<std::net::IpAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let ip = match b.len() {
        4 => IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(b).ok()?)),
        16 => {
            let v6 = Ipv6Addr::from(<[u8; 16]>::try_from(b).ok()?);
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
