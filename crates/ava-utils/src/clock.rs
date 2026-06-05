// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Injectable clock — the ONLY place wall-clock time may be read (hazard #5).
//!
//! Ported from `specs/24-determinism-and-clock.md` §B.1. Mirrors Go
//! `utils/timer/mockable.Clock`. All consensus/codec/VM time reads go through a
//! `Clock`; never call `SystemTime::now()` / `Instant::now()` directly. This
//! module is the determinism allowlist for those calls.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Mirrors Go `mockable.MaxTime` (`time.Unix(1<<63-62135596801, 0)`), used as a
/// "never" sentinel for deadlines. Nanoseconds dropped, matching Go.
pub const MAX_UNIX_SECS: u64 = (1u64 << 63) - 62_135_596_801;

/// A handle to "the current time", injected wherever Go injects `mockable.Clock`.
/// Object-safe so it can travel as `Arc<dyn Clock>`. All consensus/codec/VM time
/// reads go through this; never call `SystemTime::now()` directly (hazard #5).
pub trait Clock: Send + Sync {
    /// Wall-clock instant. Go `Clock.Time()`. May move backward (NTP step) —
    /// callers needing monotonicity use [`Clock::monotonic`] (see §B.4).
    fn now(&self) -> SystemTime;

    /// Wall instant truncated to whole seconds. Go `Clock.UnixTime()`.
    fn unix_time(&self) -> SystemTime {
        let secs = self.unix();
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// Unix timestamp in seconds, clamped to >= 0. Go `Clock.Unix()`
    /// (`max(t.Unix(), 0)` then `uint64`).
    fn unix(&self) -> u64 {
        self.now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) // pre-epoch clamps to 0, matching Go's max(.,0)
    }

    /// Elapsed wall time since `earlier`, saturating at zero (Go often does
    /// `clock.Time().Sub(t0)`; e.g. proposervm `timeSinceBootstrapping`).
    fn since(&self, earlier: SystemTime) -> Duration {
        self.now().duration_since(earlier).unwrap_or(Duration::ZERO)
    }

    /// Monotonic reading for latency/timeout measurement (see §B.4). The real
    /// clock backs this with `Instant`; the mock derives it from advances.
    fn monotonic(&self) -> tokio::time::Instant;
}

/// Production clock: reads the OS wall clock and a process-monotonic instant.
/// This is the ONLY type allowed to touch `SystemTime::now`/`Instant::now`
/// (determinism-allow: this is the clock crate; xtask allowlists this module).
#[derive(Clone, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> SystemTime {
        SystemTime::now() // determinism-allow: ava-utils::clock
    }
    fn monotonic(&self) -> tokio::time::Instant {
        tokio::time::Instant::now() // determinism-allow: ava-utils::clock; honors tokio pause() in tests (§B.2)
    }
}

/// Test clock: replaces Go `Clock.Set`/`Sync`. Faked time is shared so a test
/// can hold the `Arc<dyn Clock>` and still advance it.
#[derive(Clone, Default)]
pub struct MockClock {
    inner: Arc<parking_lot::Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    /// `Some` == faked (Go `faked == true`); `None` == fall through to wall clock.
    faked: Option<SystemTime>,
    /// Monotonic base; advanced by `advance`, independent of the faked wall time.
    mono: Option<tokio::time::Instant>,
}

impl MockClock {
    /// Construct already faked at a fixed instant — the common test setup.
    #[must_use]
    pub fn at(t: SystemTime) -> Self {
        let s = Self::default();
        s.set(t);
        s
    }

    /// Go `Clock.Set(t)` — pin the clock to `t` and mark it faked.
    pub fn set(&self, t: SystemTime) {
        let mut g = self.inner.lock();
        g.faked = Some(t);
    }

    /// Go `Clock.Sync()` — stop faking, fall back to the wall clock.
    pub fn sync(&self) {
        self.inner.lock().faked = None;
    }

    /// Move faked time forward by `d` (no Go equivalent method; Go tests call
    /// `Set(old.Add(d))`). Also advances the monotonic reading by `d`.
    pub fn advance(&self, d: Duration) {
        let mut g = self.inner.lock();
        if let Some(t) = g.faked {
            g.faked = Some(t + d);
        }
        if let Some(m) = g.mono {
            g.mono = Some(m + d);
        }
    }
}

impl Clock for MockClock {
    fn now(&self) -> SystemTime {
        match self.inner.lock().faked {
            Some(t) => t,
            None => SystemTime::now(), // determinism-allow: ava-utils::clock
        }
    }
    fn monotonic(&self) -> tokio::time::Instant {
        let mut g = self.inner.lock();
        *g.mono.get_or_insert_with(tokio::time::Instant::now) // determinism-allow: ava-utils::clock
    }
}
