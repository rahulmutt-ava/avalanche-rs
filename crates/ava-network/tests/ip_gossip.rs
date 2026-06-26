// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.17 — IP-tracker + PeerList/GetPeerList gossip (bloom + salt) + verified
//! `ClaimedIpPort` (`specs/05` §3.5/§3.7).

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ava_crypto::bls::LocalSigner;
use ava_message::proto::p2p;
use ava_network::Identity;
use ava_network::network::bloom::{Filter, ReadFilter, hash};
use ava_network::network::ip_tracker::{IpTracker, gossip_id};
use ava_network::peer::ip::UnsignedIp;
use ava_types::node_id::NodeId;

/// Build a valid `ClaimedIpPort` signed by a fresh staking identity.
fn signed_claimed_ip(timestamp: u64) -> (NodeId, p2p::ClaimedIpPort) {
    let identity = Identity::generate().expect("identity");
    let cert = ava_crypto::staking::parse_certificate(identity.cert_der()).expect("cert");
    let node_id = ava_crypto::staking::node_id_from_cert(&cert.raw);
    let bls = LocalSigner::generate().expect("bls");

    let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    let port = 9651u16;
    let unsigned = UnsignedIp::new(ip, port, timestamp);
    let tls_key = identity.tls_signing_key().expect("tls key");
    let signed = unsigned.sign(&tls_key, &bls).expect("sign");

    let claimed = p2p::ClaimedIpPort {
        x509_certificate: bytes::Bytes::copy_from_slice(identity.cert_der()),
        ip_addr: bytes::Bytes::copy_from_slice(&ip_as16(ip)),
        ip_port: u32::from(port),
        timestamp,
        signature: bytes::Bytes::copy_from_slice(signed.tls_signature()),
        tx_id: bytes::Bytes::copy_from_slice(&[0u8; 32]),
    };
    (node_id, claimed)
}

fn ip_as16(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

/// A one-hash empty bloom filter blob (`num_hashes=1 || seed(0) || 0x00`).
fn empty_filter() -> Vec<u8> {
    let mut v = vec![1u8];
    v.extend_from_slice(&0u64.to_be_bytes());
    v.push(0x00);
    v
}

/// `peers()` excludes a node already present in the requester's bloom filter.
#[test]
fn peers_excludes_known_via_bloom() {
    let tracker = IpTracker::new();
    let (node, claimed) = signed_claimed_ip(1_700_000_000);
    tracker
        .add_claimed_ip_port(&claimed, 1_700_000_000)
        .expect("track valid claim");

    let salt = [0u8; 4];

    // With an empty filter, the claim is returned (not yet known).
    let returned = tracker.peers(&empty_filter(), &salt).expect("peers");
    assert_eq!(returned.len(), 1, "unknown peer is gossiped");

    // Build a filter that contains the gossip_id of this claim (keyed on
    // gossip_id, not raw node bytes) — so peers() should exclude it when
    // checking gossip_id, but NOT when checking raw node.as_bytes().
    let gid = gossip_id(&node, 1_700_000_000);
    let mut filter = Filter::new(1, 64).expect("Filter::new");
    filter.add_key(&gid, &salt);
    let filter_bytes = filter.marshal();
    let f = ReadFilter::parse(&filter_bytes).expect("parse");
    assert!(f.contains(hash(&gid, &salt)), "gossip_id is in the filter");
    assert!(
        !f.contains(hash(node.as_bytes(), &salt)),
        "raw node bytes are NOT in the filter (ensures RED before fix)"
    );
    let returned = tracker.peers(&filter_bytes, &salt).expect("peers");
    assert!(returned.is_empty(), "known peer (by gossip id) is excluded");
}

/// A `ClaimedIpPort` with a bad signature is rejected; a valid one is tracked.
#[test]
fn claimed_ip_port_verified_before_track() {
    let tracker = IpTracker::new();

    let (_node, mut bad) = signed_claimed_ip(1_700_000_000);
    // Corrupt the signature.
    let mut sig = bad.signature.to_vec();
    if let Some(b) = sig.last_mut() {
        *b ^= 0xff;
    }
    bad.signature = bytes::Bytes::from(sig);
    assert!(
        tracker.add_claimed_ip_port(&bad, 1_700_000_000).is_err(),
        "bad signature rejected"
    );
    assert_eq!(tracker.len(), 0);

    let (node, good) = signed_claimed_ip(1_700_000_000);
    let tracked = tracker
        .add_claimed_ip_port(&good, 1_700_000_000)
        .expect("good claim tracked");
    assert_eq!(tracked, node);
    assert!(tracker.contains(&node));
}

/// A salt longer than `maxBloomSaltLen` (32) is rejected (cross-checks §1.4).
#[test]
fn bloom_salt_over_max_rejected() {
    let tracker = IpTracker::new();
    let salt = vec![0u8; 33];
    assert!(tracker.peers(&empty_filter(), &salt).is_err());
}

fn _unused(_: SocketAddr) {}
