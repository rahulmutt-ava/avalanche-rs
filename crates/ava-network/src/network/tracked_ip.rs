// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `TrackedIp` — a node's most-recent claimed IP plus its reconnect-backoff
//! state (`specs/05` §3.5).
//!
//! Mirrors Go `network/tracked_ip.go`. The dialer keeps reconnecting to a
//! tracked IP with exponential backoff between
//! `DefaultNetworkInitialReconnectDelay` (1s) and
//! `DefaultNetworkMaxReconnectDelay` (1m).

use std::net::SocketAddr;
use std::time::Duration;

/// Initial reconnect delay (Go `DefaultNetworkInitialReconnectDelay`).
pub const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Maximum reconnect delay (Go `DefaultNetworkMaxReconnectDelay`).
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

/// A claimed `(ip, port, timestamp)` for a node, plus the signature bytes and
/// the X.509 cert that the claim was authenticated with.
#[derive(Debug, Clone)]
pub struct ClaimedIp {
    /// The claimed address.
    pub addr: SocketAddr,
    /// The Unix-seconds timestamp of the claim.
    pub timestamp: u64,
    /// The TLS signature over the signed-IP bytes.
    pub tls_signature: Vec<u8>,
    /// The peer's DER-encoded X.509 leaf certificate.
    pub cert_der: Vec<u8>,
    /// The P-Chain transaction id that added this peer to the validator set.
    pub tx_id: ava_types::id::Id,
}

/// The dialer's reconnect state for a tracked node.
#[derive(Debug, Clone)]
pub struct TrackedIp {
    /// The most-recent claimed address (the dial target).
    pub addr: SocketAddr,
    /// The current backoff delay (grows up to [`MAX_RECONNECT_DELAY`]).
    pub delay: Duration,
}

impl TrackedIp {
    /// A fresh tracked IP at the initial backoff.
    #[must_use]
    pub fn new(addr: SocketAddr) -> TrackedIp {
        TrackedIp {
            addr,
            delay: INITIAL_RECONNECT_DELAY,
        }
    }

    /// Increase the backoff after a failed dial, capped at the maximum
    /// (Go: `delay = min(2*delay, max)` with a small jitter — jitter omitted).
    pub fn increase_delay(&mut self) {
        let doubled = self.delay.saturating_mul(2);
        self.delay = doubled.min(MAX_RECONNECT_DELAY);
    }

    /// Reset the backoff (after a successful connection).
    pub fn reset_delay(&mut self) {
        self.delay = INITIAL_RECONNECT_DELAY;
    }
}
