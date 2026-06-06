// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The outbound `Dialer` (`specs/05` §3.4).
//!
//! Mirrors Go `network/dialer/dialer.go`: a TCP connect with a connection
//! timeout (`DefaultOutboundConnectionTimeout = 30s`) gated by a token-bucket
//! dial rate-limiter (`DefaultOutboundConnectionThrottlingRps = 50`).
//!
//! The rate-limiter is a small hand-rolled token bucket guarded by a
//! `parking_lot::Mutex` rather than pulling in `governor` — the same
//! dependency-minimizing choice the throttlers made (`specs/05` §5 findings).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::net::TcpStream;

use crate::error::{Error, Result};

/// Default outbound connection timeout (Go `DefaultOutboundConnectionTimeout`).
pub const OUTBOUND_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
/// Default outbound dial rate (Go `DefaultOutboundConnectionThrottlingRps`).
pub const OUTBOUND_THROTTLING_RPS: u32 = 50;

/// A token-bucket rate limiter (refills `rps` tokens per second up to `rps`).
#[derive(Debug)]
struct TokenBucket {
    /// Tokens added per second (== burst capacity).
    rps: f64,
    /// Available tokens.
    tokens: f64,
    /// Last refill instant.
    last: Instant,
}

impl TokenBucket {
    fn new(rps: u32) -> Self {
        let rps = f64::from(rps.max(1));
        Self {
            rps,
            tokens: rps,
            last: Instant::now(),
        }
    }

    /// The duration to wait before a token is available (zero if one is ready),
    /// consuming a token when granting immediately.
    fn reserve(&mut self, now: Instant) -> Duration {
        // Refill.
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rps).min(self.rps);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Duration::ZERO
        } else {
            let deficit = 1.0 - self.tokens;
            self.tokens = 0.0;
            Duration::from_secs_f64(deficit / self.rps)
        }
    }
}

/// Dials outbound TCP connections with a timeout + dial-rate throttle.
#[derive(Debug)]
pub struct Dialer {
    timeout: Duration,
    bucket: Mutex<TokenBucket>,
}

impl Default for Dialer {
    fn default() -> Self {
        Self::new(OUTBOUND_CONNECTION_TIMEOUT, OUTBOUND_THROTTLING_RPS)
    }
}

impl Dialer {
    /// Build a dialer with the given connect `timeout` and throttle `rps`.
    #[must_use]
    pub fn new(timeout: Duration, rps: u32) -> Self {
        Self {
            timeout,
            bucket: Mutex::new(TokenBucket::new(rps)),
        }
    }

    /// Dial `addr`, respecting the rate limit and connect timeout.
    ///
    /// # Errors
    /// [`Error::Io`] on a connect failure or timeout.
    pub async fn dial(&self, addr: SocketAddr) -> Result<TcpStream> {
        // Throttle (no sync lock across the await: compute the wait, drop the
        // lock, then sleep).
        let wait = self.bucket.lock().reserve(Instant::now());
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }

        match tokio::time::timeout(self.timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => Err(Error::Io(e.to_string())),
            Err(_) => Err(Error::Io(format!("dial timeout to {addr}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_grants_then_throttles() {
        let mut b = TokenBucket::new(2);
        let t0 = Instant::now();
        // Two immediate grants (burst == rps).
        assert_eq!(b.reserve(t0), Duration::ZERO);
        assert_eq!(b.reserve(t0), Duration::ZERO);
        // Third must wait (~0.5s at 2 rps).
        let wait = b.reserve(t0);
        assert!(wait > Duration::ZERO);
    }
}
