// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Peer version compatibility (version-vs-upgrade-time).
//!
//! Mirrors `version/compatibility.go` from Go. The checker decides whether a
//! peer's reported version is compatible with ours, selecting the appropriate
//! minimum-compatible floor based on whether the upgrade time has passed.
//!
//! Owning spec: `specs/03-core-primitives.md` §5.1, `specs/26-versioning-and-compatibility.md` §3.

use std::time::SystemTime;

use crate::application::Application;

/// A clock abstraction that can be swapped for tests.
///
/// Mirrors the `mockable.Clock` injection pattern used in Go's `Compatibility`
/// (`version/compatibility.go`).
pub trait Clock {
    /// Returns the current time.
    fn now(&self) -> SystemTime;
}

/// A real wall-clock implementation of [`Clock`].
#[derive(Clone, Debug, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A mock clock for testing, holding a fixed time.
#[derive(Clone, Debug)]
pub struct MockClock {
    now: SystemTime,
}

impl MockClock {
    /// Creates a new mock clock fixed at the given time.
    pub fn new(now: SystemTime) -> Self {
        Self { now }
    }
}

impl Clock for MockClock {
    fn now(&self) -> SystemTime {
        self.now
    }
}

/// Decides whether a peer's version is compatible with this node's version.
///
/// Mirrors `version.Compatibility` from Go (`version/compatibility.go`).
///
/// The invariant is: `current >= min_compatible_after_upgrade >= min_compatible`.
///
/// The two-clause rule (mirrors Go `Compatibility.Compatible`):
/// 1. If our `current.major < peer.major` → incompatible (we cannot understand
///    a newer major).
/// 2. Select the floor: if the clock is **before** `upgrade_time`, the floor is
///    `min_compatible`; otherwise `min_compatible_after_upgrade`. The peer must be
///    `>= floor` to be compatible.
pub struct Compatibility<C: Clock = RealClock> {
    /// Our current version — the version this node reports in the handshake.
    pub current: Application,
    /// The minimum acceptable peer version once the upgrade time has passed.
    /// Mirrors `MinimumCompatibleVersion` in Go.
    pub min_compatible_after_upgrade: Application,
    /// The minimum acceptable peer version before the upgrade time.
    /// Mirrors `PrevMinimumCompatibleVersion` in Go.
    pub min_compatible: Application,
    /// The fork time that switches the floor from `min_compatible` to
    /// `min_compatible_after_upgrade`. Threaded in from the network config.
    pub upgrade_time: SystemTime,
    /// The clock (real or mock). Injected for tests.
    pub clock: C,
}

impl<C: Clock> Compatibility<C> {
    /// Constructs a `Compatibility` with an explicit clock.
    pub fn with_clock(
        current: Application,
        min_compatible_after_upgrade: Application,
        min_compatible: Application,
        upgrade_time: SystemTime,
        clock: C,
    ) -> Self {
        Self {
            current,
            min_compatible_after_upgrade,
            min_compatible,
            upgrade_time,
            clock,
        }
    }

    /// Returns `true` iff the peer is compatible with this node.
    ///
    /// Mirrors Go `Compatibility.Compatible(peer *version.Application) error`:
    ///
    /// - Clause 1: `current.major < peer.major` → incompatible.
    /// - Clause 2: `peer >= floor` where `floor` is selected by the clock.
    pub fn compatible(&self, peer: &Application) -> bool {
        // Clause 1: reject if peer is on a newer major than us.
        if self.current.major < peer.major {
            return false;
        }
        // Clause 2: select the floor and check peer >= floor.
        let floor = if self.clock.now() < self.upgrade_time {
            &self.min_compatible
        } else {
            &self.min_compatible_after_upgrade
        };
        peer >= floor
    }
}

impl Compatibility<RealClock> {
    /// Constructs a `Compatibility` using the real system clock.
    ///
    /// Mirrors Go `version.GetCompatibility(upgrade_time)`. The `upgrade_time`
    /// is the network fork time that gates the floor switch; it is threaded in
    /// by `ava-network` (mirrors `network.go`'s `minCompatibleTime`).
    pub fn new(
        current: Application,
        min_compatible_after_upgrade: Application,
        min_compatible: Application,
        upgrade_time: SystemTime,
    ) -> Self {
        Self {
            current,
            min_compatible_after_upgrade,
            min_compatible,
            upgrade_time,
            clock: RealClock,
        }
    }
}

/// Constructs a [`Compatibility`] with the built-in version constants and the real clock.
///
/// Mirrors Go `version.GetCompatibility(upgradeTime)` from `version/constants.go`.
pub fn get_compatibility(upgrade_time: SystemTime) -> Compatibility<RealClock> {
    Compatibility::new(
        crate::application::CURRENT.clone(),
        crate::application::MINIMUM_COMPATIBLE.clone(),
        crate::application::PREV_MINIMUM_COMPATIBLE.clone(),
        upgrade_time,
    )
}
