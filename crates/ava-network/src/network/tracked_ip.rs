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
use std::time::{Duration, Instant};

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
    /// The earliest instant the dialer may (re)attempt this IP.
    pub next_attempt: Instant,
}

impl TrackedIp {
    /// A fresh tracked IP at the initial backoff.
    ///
    /// `next_attempt` is set to a point in the past so the first dial is
    /// immediate regardless of when `should_dial` is first called.
    #[must_use]
    pub fn new(addr: SocketAddr) -> TrackedIp {
        // Subtract the initial delay so the fresh IP is always dial-ready.
        let now = Instant::now();
        let next_attempt = now.checked_sub(INITIAL_RECONNECT_DELAY).unwrap_or(now);
        TrackedIp {
            addr,
            delay: INITIAL_RECONNECT_DELAY,
            next_attempt,
        }
    }

    /// Whether the dialer may attempt this IP at `now` (backoff window elapsed).
    #[must_use]
    pub fn should_dial(&self, now: Instant) -> bool {
        now >= self.next_attempt
    }

    /// Record a (failed/pending) dial attempt at `now`: gate the next attempt by
    /// the current delay, then grow the backoff.
    pub fn record_attempt(&mut self, now: Instant) {
        self.next_attempt = now.checked_add(self.delay).unwrap_or(now);
        self.increase_delay();
    }

    /// Record a successful connection at `now`: reset the backoff and re-open dialing.
    pub fn record_success(&mut self, now: Instant) {
        self.reset_delay();
        self.next_attempt = now;
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};

    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:9651".parse().expect("addr")
    }

    #[test]
    fn backoff_gates_redials_and_grows_then_resets() {
        let t0 = Instant::now();
        let mut ip = TrackedIp::new(addr());
        // Fresh: dial immediately.
        assert!(ip.should_dial(t0), "fresh tracked ip should dial");

        // After an attempt, the next dial is gated by the (initial 1s) delay,
        // and the delay doubles for the following attempt.
        ip.record_attempt(t0);
        assert!(!ip.should_dial(t0), "must wait out the backoff window");
        assert!(
            !ip.should_dial(t0 + Duration::from_millis(999)),
            "still inside the 1s window"
        );
        assert!(
            ip.should_dial(t0 + Duration::from_secs(1)),
            "dial once the window elapses"
        );

        // Second failed attempt: window is now 2s.
        let t1 = t0 + Duration::from_secs(1);
        ip.record_attempt(t1);
        assert!(
            !ip.should_dial(t1 + Duration::from_millis(1999)),
            "2s window"
        );
        assert!(
            ip.should_dial(t1 + Duration::from_secs(2)),
            "after 2s window"
        );

        // Cap at MAX_RECONNECT_DELAY.
        for _ in 0..10 {
            let now = ip.next_attempt;
            ip.record_attempt(now);
        }
        assert_eq!(ip.delay, MAX_RECONNECT_DELAY, "backoff caps at the maximum");

        // A successful connection resets the backoff and re-opens dialing.
        let t2 = ip.next_attempt;
        ip.record_success(t2);
        assert_eq!(ip.delay, INITIAL_RECONNECT_DELAY, "success resets backoff");
        assert!(ip.should_dial(t2), "success re-opens dialing");
    }
}
