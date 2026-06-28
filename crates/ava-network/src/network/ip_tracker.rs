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
use crate::network::bloom::{Filter, ReadFilter, estimate_count, optimal_parameters};
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

/// Bloom false-positive target (Go `targetFalsePositiveProbability`).
const TARGET_FALSE_POSITIVE_PROBABILITY: f64 = 0.001;
/// Minimum bloom element-count estimate (Go `minCountEstimate`).
const MIN_COUNT_ESTIMATE: usize = 128;
/// Max bloom additions per node (Go `maxIPEntriesPerNode`).
const MAX_IP_ENTRIES_PER_NODE: usize = 2;
/// Bloom salt length (Go `saltSize`).
const SALT_SIZE: usize = 32;

/// Tracks learned validator IPs + answers peer-list-gossip queries.
pub struct IpTracker {
    inner: Mutex<Inner>,
}

struct Inner {
    /// node -> its most-recent verified claim (sorted for determinism).
    claims: BTreeMap<NodeId, ClaimedIp>,
    /// The peer-list-gossip bloom filter (Go `ipTracker.bloom`).
    bloom: Filter,
    /// The 32-byte bloom salt (Go `ipTracker.bloomSalt`).
    bloom_salt: Vec<u8>,
    /// Per-node count of bloom additions (Go `ipTracker.bloomAdditions`),
    /// capped at `MAX_IP_ENTRIES_PER_NODE`.
    bloom_additions: BTreeMap<NodeId, usize>,
}

impl Default for IpTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl IpTracker {
    /// A fresh tracker with a seeded (empty) bloom filter + salt.
    #[must_use]
    pub fn new() -> IpTracker {
        let (bloom, bloom_salt) = build_bloom(&BTreeMap::new());
        IpTracker {
            inner: Mutex::new(Inner {
                claims: BTreeMap::new(),
                bloom,
                bloom_salt,
                bloom_additions: BTreeMap::new(),
            }),
        }
    }

    /// The current bloom filter bytes + salt for an outbound `Handshake` /
    /// `GetPeerList` (Go `ipTracker.Bloom()` / the peer's `KnownPeers()`).
    #[must_use]
    pub fn bloom(&self) -> (Vec<u8>, Vec<u8>) {
        let inner = self.inner.lock();
        (inner.bloom.marshal(), inner.bloom_salt.clone())
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

        let tx_id = ava_types::id::Id::from_slice(&claimed.tx_id)
            .unwrap_or_else(|_| ava_types::id::Id::default());

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
            let additions = inner.bloom_additions.get(&node_id).copied().unwrap_or(0);
            if additions < MAX_IP_ENTRIES_PER_NODE {
                let gid = gossip_id(&node_id, claimed.timestamp);
                let salt = inner.bloom_salt.clone();
                inner.bloom.add_key(&gid, &salt);
                inner
                    .bloom_additions
                    .insert(node_id, additions.saturating_add(1));
            }
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
    pub fn peers(&self, filter_bytes: &[u8], salt: &[u8]) -> Result<Vec<p2p::ClaimedIpPort>> {
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
            // Skip an IP the requester already knows (its gossip id is in the
            // bloom filter, salted) — Go keys on ClaimedIPPort.GossipID.
            if filter.contains_key(&gossip_id(node, claim.timestamp), salt) {
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

/// Build a fresh bloom filter + salt sized for `claims` (Go
/// `ipTracker.resetBloom`). Seeds every claim that has a verified IP
/// (`cert_der` non-empty) keyed on its `gossip_id`.
fn build_bloom(claims: &BTreeMap<NodeId, ClaimedIp>) -> (Filter, Vec<u8>) {
    let count = MAX_IP_ENTRIES_PER_NODE
        .saturating_mul(claims.len())
        .max(MIN_COUNT_ESTIMATE);
    let (num_hashes, num_entries) = optimal_parameters(count, TARGET_FALSE_POSITIVE_PROBABILITY);
    let mut filter = Filter::new(num_hashes, num_entries).unwrap_or_else(|_| Filter::minimal());

    let mut salt = vec![0u8; SALT_SIZE];
    crate::network::bloom::fill_random_pub(&mut salt);

    for (node, claim) in claims {
        // Manually-tracked entries (no verified cert) are not bloom'd
        // (Go `trackedNode.ip == nil` skip).
        if claim.cert_der.is_empty() {
            continue;
        }
        let gid = gossip_id(node, claim.timestamp);
        filter.add_key(&gid, &salt);
    }
    // `estimate_count` is computed by the deferred ResetBloom timer; not needed
    // until auto-reset lands (see plan follow-ups). Reference call retained:
    let _ = estimate_count(num_hashes, num_entries, TARGET_FALSE_POSITIVE_PROBABILITY);
    (filter, salt)
}

/// `ClaimedIPPort.GossipID` (Go `utils/ips/claimed_ip_port.go`): the bloom key
/// for a tracked peer. Go hashes a PRE-SIZED `preimageLen = ids.IDLen(32) +
/// LongLen(8) = 40`-byte buffer — the 20-byte NodeID at `[0..20)`, the 8-byte
/// big-endian timestamp at `[20..28)`, and 12 trailing zero bytes at `[28..40)`
/// — i.e. `sha256(node_id || timestamp_be || 12 zero bytes)`. The packer never
/// reslices to the write offset, so all 40 bytes are hashed.
#[must_use]
pub fn gossip_id(node: &NodeId, timestamp: u64) -> [u8; 32] {
    /// Go `preimageLen` = `ids.IDLen(32) + wrappers.LongLen(8)`.
    const PREIMAGE_LEN: usize = 40;
    let mut preimage = Vec::with_capacity(PREIMAGE_LEN);
    preimage.extend_from_slice(node.as_bytes()); // [0..20)
    preimage.extend_from_slice(&timestamp.to_be_bytes()); // [20..28)
    preimage.resize(PREIMAGE_LEN, 0); // pad [28..40) with zeros
    ava_crypto::hashing::sha256(&preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_bloom_is_go_parseable_and_311_bytes() {
        let tracker = IpTracker::new();
        let (filter, salt) = tracker.bloom();
        assert_eq!(salt.len(), 32, "salt is saltSize bytes");
        // count=128, fpp=0.001 -> (10, 230) -> 1 + 10*8 + 230 = 311.
        assert_eq!(filter.len(), 311, "fresh empty filter marshal length");
        let rf = crate::network::bloom::ReadFilter::parse(&filter)
            .expect("fresh bloom parses (Go bloom.Parse equivalent)");
        assert!(
            !rf.contains_key(&[0u8; 20], &salt),
            "empty filter contains nothing"
        );
    }

    #[test]
    fn manually_tracked_node_is_not_in_bloom() {
        let tracker = IpTracker::new();
        let node = NodeId::from_slice(&[3u8; 20]).expect("node id");
        tracker.manually_track(node, "127.0.0.1:9651".parse().expect("addr"));
        let (filter, salt) = tracker.bloom();
        let rf = crate::network::bloom::ReadFilter::parse(&filter).expect("parse");
        // manually-tracked (no verified IP) is not bloom'd (Go ip == nil skip).
        assert!(
            !rf.contains_key(&gossip_id(&node, 0), &salt),
            "manual track not in bloom"
        );
    }

    #[test]
    fn gossip_id_matches_go_oracle() {
        // Pinned against avalanchego ips.ClaimedIPPort.GossipID computed by the
        // real Go wrappers.Packer + hashing.ComputeHash256Array for
        // node=[7;20], timestamp=1_700_000_000 (preimage = node(20) ||
        // ts_be(8) || 12 zero bytes = 40 bytes).
        let node = NodeId::from_slice(&[7u8; 20]).expect("node id");
        let expected: [u8; 32] = [
            0x8c, 0x6a, 0x26, 0x75, 0xfd, 0xa1, 0x2b, 0xc1, 0xc7, 0x32, 0x46, 0xbe, 0x2c, 0xd7,
            0xed, 0xd7, 0x0e, 0x45, 0x2d, 0xf0, 0x94, 0x90, 0xf2, 0x33, 0xe9, 0xa4, 0x01, 0x73,
            0x35, 0x80, 0xd2, 0xdc,
        ];
        assert_eq!(
            gossip_id(&node, 1_700_000_000),
            expected,
            "gossip_id matches Go GossipID"
        );
    }

    #[test]
    fn gossip_id_differs_by_timestamp() {
        let node = NodeId::from_slice(&[7u8; 20]).expect("node id");
        assert_ne!(
            gossip_id(&node, 1),
            gossip_id(&node, 2),
            "timestamp affects gossip id"
        );
    }
}
