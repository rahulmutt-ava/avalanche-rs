// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Inbound connection-upgrade throttler.
//!
//! Port of `network/throttling/inbound_conn_upgrade_throttler.go`. Decides
//! whether an inbound TCP connection from a given IP should be *upgraded* (the
//! TLS handshake + peer handshake performed). If [`InboundConnUpgradeThrottler::should_upgrade`]
//! returns `false`, the caller must drop the connection.
//!
//! Two gates, both mirroring Go:
//!
//! 1. **Per-IP cooldown** (`UpgradeCooldown`, default `10s`): a given IP may be
//!    upgraded at most once per cooldown window. The IP is keyed by *address
//!    only* (not port) to mitigate DoS from many ports on one host.
//! 2. **Global rate limit** (`MaxRecentConnsUpgraded`, default `256`): at most
//!    this many upgrades may be admitted within one cooldown window across all
//!    IPs. Go implements this with a bounded channel of size
//!    `MaxRecentConnsUpgraded`; this port uses an equivalent count of
//!    not-yet-expired admissions.
//!
//! Loopback addresses are never rate-limited (Go `addr.IsLoopback()`).
//!
//! ## Differences from Go
//!
//! Go runs a background `Dispatch` goroutine that pops expired IPs off a
//! channel using a `mockable.Clock`. This port is fully passive: expiry is
//! computed lazily on each call from the supplied `now: Instant`, so there is
//! no background task to start/stop ([`Dispatch`]/`Stop` have no Rust
//! analogue). State is guarded by a single [`parking_lot::Mutex`], matching
//! Go's single `sync.Mutex` (DashMap is unnecessary and would not preserve the
//! global-count semantics under one critical section).
//!
//! The clock is injected the established-repo way: the public
//! [`InboundConnUpgradeThrottler::should_upgrade`] calls
//! [`InboundConnUpgradeThrottler::should_upgrade_at`] with `Instant::now()`,
//! and tests drive `should_upgrade_at` with a synthetic clock.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Throttles *upgrading* of inbound connections per source IP.
///
/// Construct with [`InboundConnUpgradeThrottler::new`]. Cheap to clone-share
/// behind an `Arc` by the caller; the type itself is `Send + Sync`.
#[derive(Debug)]
pub struct InboundConnUpgradeThrottler {
    /// Minimum time between successful upgrades from the same IP. If zero,
    /// per-IP throttling is disabled (mirrors Go `UpgradeCooldown <= 0`).
    cooldown: Duration,
    /// Maximum number of upgrades admitted within one cooldown window across
    /// all IPs. If zero, global throttling is disabled (mirrors Go
    /// `MaxRecentConnsUpgraded <= 0`).
    max_recent: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// IP -> the [`Instant`] at which this IP's cooldown elapses (i.e. the time
    /// of the admitting call plus `cooldown`). An entry is "recent" while
    /// `now < cooldown_elapsed_at`.
    recent: HashMap<IpAddr, Instant>,
}

impl InboundConnUpgradeThrottler {
    /// Creates a throttler with the given per-IP `cooldown` and global
    /// `max_recent` admissions-per-window.
    ///
    /// Defaults from `utils/constants/networking.go`:
    /// `DefaultInboundConnUpgradeThrottlerCooldown = 10s`,
    /// `DefaultInboundThrottlerMaxConnsPerSec = 256`.
    ///
    /// If either bound is zero the corresponding gate is disabled (every
    /// connection is upgraded), matching Go's `noInboundConnUpgradeThrottler`
    /// fallback.
    #[must_use]
    pub fn new(cooldown: Duration, max_recent: usize) -> Self {
        Self {
            cooldown,
            max_recent,
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Returns whether an inbound connection from `ip` should be upgraded,
    /// using the real clock. See [`Self::should_upgrade_at`].
    #[must_use]
    pub fn should_upgrade(&self, ip: IpAddr) -> bool {
        self.should_upgrade_at(ip, Instant::now())
    }

    /// Returns whether an inbound connection from `ip` should be upgraded as of
    /// `now`. On `true`, the IP is recorded so subsequent calls within
    /// `cooldown` are rejected.
    ///
    /// Loopback IPs are always allowed and never recorded. If both gates are
    /// disabled (zero cooldown or zero `max_recent`), always returns `true`.
    #[must_use]
    pub fn should_upgrade_at(&self, ip: IpAddr, now: Instant) -> bool {
        // Don't rate-limit loopback IPs (Go `addr.IsLoopback()`).
        if ip.is_loopback() {
            return true;
        }
        // Disabled gates: upgrade everything (Go `noInboundConnUpgradeThrottler`).
        if self.cooldown.is_zero() || self.max_recent == 0 {
            return true;
        }

        let mut inner = self.inner.lock();

        // Drop any entries whose cooldown has elapsed (lazy equivalent of Go's
        // Dispatch goroutine popping expired IPs).
        inner.recent.retain(|_, &mut elapsed_at| now < elapsed_at);

        // Per-IP cooldown: recently upgraded from this IP.
        if inner.recent.contains_key(&ip) {
            return false;
        }

        // Global rate limit: at most `max_recent` live admissions per window.
        if inner.recent.len() >= self.max_recent {
            return false;
        }

        // Admit: record the cooldown deadline for this IP.
        let elapsed_at = now.checked_add(self.cooldown).unwrap_or(now);
        inner.recent.insert(ip, elapsed_at);
        true
    }
}
