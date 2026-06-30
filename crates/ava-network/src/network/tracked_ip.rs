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
    /// A per-instance jitter seed derived from the address and creation time.
    /// Used to break the lockstep when two nodes mutually dial each other:
    /// each `TrackedIp` gets a unique seed so their jittered retry windows
    /// differ even when `record_attempt` is called at the same wall-clock time.
    jitter_seed: u32,
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
        // Seed the per-instance jitter from the address port (unique per peer in
        // a test or production run) XOR'd with the low bits of the current
        // nanosecond timestamp for additional entropy. This ensures two
        // `TrackedIp`s created for different peers at the same instant have
        // different jitter, breaking mutual-dial retry lockstep.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let port_seed = u32::from(addr.port());
        let jitter_seed = nanos ^ port_seed;
        TrackedIp {
            addr,
            delay: INITIAL_RECONNECT_DELAY,
            next_attempt,
            jitter_seed,
        }
    }

    /// Whether the dialer may attempt this IP at `now` (backoff window elapsed).
    #[must_use]
    pub fn should_dial(&self, now: Instant) -> bool {
        now >= self.next_attempt
    }

    /// Record a (failed/pending) dial attempt at `now`: gate the next attempt by
    /// a jittered version of the current delay, then grow the base delay.
    ///
    /// Using a jittered `next_attempt` (rather than the raw `delay`) breaks the
    /// retry synchrony of two peers that mutually dial each other and both fail
    /// simultaneously: their `record_attempt` calls happen at different instants,
    /// so the jitter (derived from nanosecond wall-clock entropy) gives each side
    /// a different `next_attempt`, and the one with the shorter wait retries
    /// first, establishing the connection before the other side fires.
    pub fn record_attempt(&mut self, now: Instant) {
        // Apply jitter to the CURRENT retry window, not just future ones.
        let jittered = self.jittered_delay();
        self.next_attempt = now.checked_add(jittered).unwrap_or(now);
        self.increase_delay();
    }

    /// Jittered copy of the current delay in `[delay, 2*delay)`, using the
    /// instance's `jitter_seed`. Advances the seed (LCG) so successive calls
    /// also differ. This ensures each `TrackedIp` instance has a unique retry
    /// window even when called at the exact same wall-clock instant.
    fn jittered_delay(&mut self) -> Duration {
        // LCG step (Knuth): advance the per-instance seed.
        self.jitter_seed = self
            .jitter_seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        // jitter_frac ∈ [0, 1) from 30 low bits.
        let jitter_frac = f64::from(self.jitter_seed & 0x3FFF_FFFF) / f64::from(0x4000_0000u32);
        // multiplier ∈ [1.0, 2.0) — mirrors Go's `(1 + rand.Float64())`.
        let multiplier = 1.0 + jitter_frac;
        let jittered = Duration::from_secs_f64(self.delay.as_secs_f64() * multiplier);
        jittered.min(MAX_RECONNECT_DELAY)
    }

    /// Record a successful connection at `now`: reset the backoff and re-open dialing.
    pub fn record_success(&mut self, now: Instant) {
        self.reset_delay();
        self.next_attempt = now;
    }

    /// Increase the base delay for the NEXT `record_attempt`, capped at the
    /// maximum. Uses the instance's `jitter_seed` (advanced by a LCG step) so
    /// successive increases are unique per `TrackedIp` instance — mirrors Go's
    /// `trackedIP.increaseDelay` with `math/rand` (non-cryptographic, `#nosec G404`).
    pub fn increase_delay(&mut self) {
        // LCG step: advance the per-instance seed.
        self.jitter_seed = self
            .jitter_seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let jitter_frac = f64::from(self.jitter_seed & 0x3FFF_FFFF) / f64::from(0x4000_0000u32);
        let multiplier = 1.0 + jitter_frac;
        let new_delay = Duration::from_secs_f64(self.delay.as_secs_f64() * multiplier);
        if new_delay > MAX_RECONNECT_DELAY {
            // Clamp to [0.75, 1.0) * MAX — Go parity.
            let frac = (3.0 + jitter_frac) / 4.0;
            self.delay = Duration::from_secs_f64(MAX_RECONNECT_DELAY.as_secs_f64() * frac);
        } else {
            self.delay = new_delay;
        }
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

        // After an attempt, the next dial is gated by the initial 1s delay.
        // The delay window is [1s, 2s) due to jitter (Go parity).
        ip.record_attempt(t0);
        assert!(!ip.should_dial(t0), "must wait out the backoff window");
        // At least 1s must elapse (the lower bound of the jittered delay).
        assert!(
            !ip.should_dial(t0 + Duration::from_millis(999)),
            "still inside the 1s window"
        );
        assert!(
            ip.should_dial(t0 + Duration::from_secs(2)),
            "dial once the window elapses (upper bound of jitter range)"
        );

        // Second failed attempt: window is now in [delay, 2*delay) where delay
        // is the jittered value from the first attempt (∈ [1s, 2s)).
        let t1 = ip.next_attempt; // the exact next_attempt after the first attempt
        ip.record_attempt(t1);
        let second_delay = ip.delay;
        assert!(
            second_delay >= INITIAL_RECONNECT_DELAY,
            "delay must grow after failure"
        );
        assert!(
            second_delay <= MAX_RECONNECT_DELAY,
            "delay must not exceed maximum"
        );

        // Cap at MAX_RECONNECT_DELAY (after many attempts).
        for _ in 0..20 {
            let now = ip.next_attempt;
            ip.record_attempt(now);
        }
        assert!(
            ip.delay <= MAX_RECONNECT_DELAY,
            "backoff caps at the maximum"
        );
        assert!(
            ip.delay >= MAX_RECONNECT_DELAY.mul_f64(0.75),
            "near-cap delay stays in [0.75 * MAX, MAX]"
        );

        // A successful connection resets the backoff and re-opens dialing.
        let t2 = ip.next_attempt;
        ip.record_success(t2);
        assert_eq!(ip.delay, INITIAL_RECONNECT_DELAY, "success resets backoff");
        assert!(ip.should_dial(t2), "success re-opens dialing");
    }
}
