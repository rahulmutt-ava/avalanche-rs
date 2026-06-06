// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The outbound builder API — `Creator` implementing [`OutboundMsgBuilder`]
//! (specs/05 §2.4), a port of `message/outbound_msg_builder.go` +
//! `message/creator.go`.
//!
//! One method per outbound op; each constructs the proto and calls
//! [`MsgBuilder::create_outbound`] with the **per-op compression decision copied
//! from Go**: handshake / ping / pong / get_peerlist / peerlist are sent
//! **uncompressed**; bulk ops (Put / Ancestors / PushQuery / App\*) are zstd.
//! Only the handshake-class ops are implemented here; the bulk/consensus ops are
//! filled in by their consuming engine milestones (see crate docs / M2.5 notes).
//!
//! ## IP encoding
//! `Handshake.ip_addr` is the **16-byte `As16` form** (`utils/ips`): an IPv4
//! address is encoded as its IPv4-mapped IPv6 form (`::ffff:a.b.c.d`), an IPv6
//! address as its 16 raw bytes. This matches the bytes an avalanchego node
//! advertises. (Go's `outbound_msg_builder.go` calls `Addr().AsSlice()`, which
//! is identical whenever the stored address is already the 16-byte/IPv4-mapped
//! form the node holds; see crate-level porting notes.)

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;

use ava_types::id::Id;

use crate::codec::{Compression, MsgBuilder, OutboundMessage};
use crate::error::Result;
use crate::proto::p2p;

/// Returns the 16-byte `As16` form of an IP address: IPv4 → IPv4-mapped IPv6,
/// IPv6 → raw octets. Mirrors Go `netip.Addr.As16`.
#[must_use]
pub fn ip_as16(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

/// The outbound message builder surface (a subset of Go's
/// `OutboundMsgBuilder`). Handshake-class ops are implemented; bulk/consensus
/// ops are deferred to their engine milestones.
pub trait OutboundMsgBuilder {
    /// Builds the `Handshake` message (the first message each side writes; sent
    /// uncompressed with `bypass_throttling = true`).
    #[allow(clippy::too_many_arguments)]
    fn handshake(
        &self,
        network_id: u32,
        my_time: u64,
        ip: SocketAddr,
        client_name: &str,
        major: u32,
        minor: u32,
        patch: u32,
        upgrade_time: u64,
        ip_signing_time: u64,
        tls_sig: &[u8],
        bls_sig: &[u8],
        tracked_subnets: &[Id],
        supported_acps: &[u32],
        objected_acps: &[u32],
        known_peers_filter: &[u8],
        known_peers_salt: &[u8],
        all_subnets: bool,
    ) -> Result<OutboundMessage>;

    /// Builds a `Ping` carrying the local primary-network uptime `[0,100]`.
    fn ping(&self, uptime: u32) -> Result<OutboundMessage>;

    /// Builds a `Pong` (empty on the modern wire).
    fn pong(&self) -> Result<OutboundMessage>;

    /// Builds a `GetPeerList` carrying the known-peers bloom filter + salt.
    fn get_peer_list(
        &self,
        filter: &[u8],
        salt: &[u8],
        all_subnets: bool,
    ) -> Result<OutboundMessage>;

    /// Builds a `PeerList` advertising the given claimed IP/port records.
    fn peer_list(&self, peers: &[p2p::ClaimedIpPort], bypass: bool) -> Result<OutboundMessage>;
}

/// Holds a shared [`MsgBuilder`] + the default outbound compression type, and
/// constructs outbound messages (`message::Creator`).
#[derive(Clone)]
pub struct Creator {
    builder: Arc<MsgBuilder>,
    /// Default compression for the bulk ops (Go uses zstd). Handshake-class ops
    /// override to `None` per the per-op table.
    compression: Compression,
}

impl Creator {
    /// Builds a `Creator` with the default outbound compression (zstd, as Go).
    #[must_use]
    pub fn new(builder: MsgBuilder) -> Self {
        Self {
            builder: Arc::new(builder),
            compression: Compression::Zstd,
        }
    }

    /// Builds a `Creator` from a shared [`MsgBuilder`] and an explicit default
    /// compression type.
    #[must_use]
    pub fn with_compression(builder: Arc<MsgBuilder>, compression: Compression) -> Self {
        Self {
            builder,
            compression,
        }
    }

    /// The default outbound compression used for bulk ops.
    #[must_use]
    pub fn compression(&self) -> Compression {
        self.compression
    }
}

impl OutboundMsgBuilder for Creator {
    fn handshake(
        &self,
        network_id: u32,
        my_time: u64,
        ip: SocketAddr,
        client_name: &str,
        major: u32,
        minor: u32,
        patch: u32,
        upgrade_time: u64,
        ip_signing_time: u64,
        tls_sig: &[u8],
        bls_sig: &[u8],
        tracked_subnets: &[Id],
        supported_acps: &[u32],
        objected_acps: &[u32],
        known_peers_filter: &[u8],
        known_peers_salt: &[u8],
        all_subnets: bool,
    ) -> Result<OutboundMessage> {
        let tracked: Vec<Bytes> = tracked_subnets
            .iter()
            .map(|id| Bytes::copy_from_slice(id.as_bytes()))
            .collect();
        let handshake = p2p::Handshake {
            network_id,
            my_time,
            ip_addr: Bytes::copy_from_slice(&ip_as16(ip.ip())),
            ip_port: u32::from(ip.port()),
            upgrade_time,
            ip_signing_time,
            ip_node_id_sig: Bytes::copy_from_slice(tls_sig),
            tracked_subnets: tracked,
            client: Some(p2p::Client {
                name: client_name.to_string(),
                major,
                minor,
                patch,
            }),
            supported_acps: supported_acps.to_vec(),
            objected_acps: objected_acps.to_vec(),
            known_peers: Some(p2p::BloomFilter {
                filter: Bytes::copy_from_slice(known_peers_filter),
                salt: Bytes::copy_from_slice(known_peers_salt),
            }),
            ip_bls_sig: Bytes::copy_from_slice(bls_sig),
            all_subnets,
        };
        let m = p2p::Message {
            message: Some(p2p::message::Message::Handshake(handshake)),
        };
        // Handshake: uncompressed, bypass_throttling = true (Go).
        self.builder.create_outbound(&m, Compression::None, true)
    }

    fn ping(&self, uptime: u32) -> Result<OutboundMessage> {
        let m = p2p::Message {
            message: Some(p2p::message::Message::Ping(p2p::Ping { uptime })),
        };
        self.builder.create_outbound(&m, Compression::None, false)
    }

    fn pong(&self) -> Result<OutboundMessage> {
        let m = p2p::Message {
            message: Some(p2p::message::Message::Pong(p2p::Pong {})),
        };
        self.builder.create_outbound(&m, Compression::None, false)
    }

    fn get_peer_list(
        &self,
        filter: &[u8],
        salt: &[u8],
        all_subnets: bool,
    ) -> Result<OutboundMessage> {
        let m = p2p::Message {
            message: Some(p2p::message::Message::GetPeerList(p2p::GetPeerList {
                known_peers: Some(p2p::BloomFilter {
                    filter: Bytes::copy_from_slice(filter),
                    salt: Bytes::copy_from_slice(salt),
                }),
                all_subnets,
            })),
        };
        // GetPeerList uses the Creator's default compression (Go
        // `outbound_msg_builder.go` passes `b.compressionType`), not bypassed.
        self.builder.create_outbound(&m, self.compression, false)
    }

    fn peer_list(&self, peers: &[p2p::ClaimedIpPort], bypass: bool) -> Result<OutboundMessage> {
        let m = p2p::Message {
            message: Some(p2p::message::Message::PeerList(p2p::PeerList {
                claimed_ip_ports: peers.to_vec(),
            })),
        };
        // PeerList uses the Creator's default compression (Go
        // `outbound_msg_builder.go` passes `b.compressionType`); bypass set by
        // caller.
        self.builder.create_outbound(&m, self.compression, bypass)
    }
}
