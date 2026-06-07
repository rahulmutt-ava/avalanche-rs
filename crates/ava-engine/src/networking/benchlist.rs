// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The per-chain [`Benchlist`] (port of `snow/networking/benchlist/`, specs
//! 06 §5.5).
//!
//! Tracks consecutive request failures per peer; once a peer's consecutive
//! failures cross a threshold it is *benched* — the sender immediately fails
//! requests to it (and skips it in sampling) for a randomized cooldown,
//! preventing the engine from stalling on an unresponsive high-stake validator.
//! A successful response resets the failure count.
//!
//! Float math / randomized durations are off the consensus-determinism path
//! (06 §5.5): benching only affects which peers we query, not which block is
//! accepted. The randomized cooldown is drawn from a seedable gonum-exact MT
//! ([`Mt19937_64`]) so tests are reproducible.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use ava_types::node_id::NodeId;
use ava_utils::rng::{Mt19937_64, Source};

/// Benchlist tuning (Go `benchlist.Config`, simplified to the consecutive-failure
/// model described in 06 §5.5).
#[derive(Clone, Debug)]
pub struct BenchlistConfig {
    /// Consecutive failures before a peer is benched.
    pub failure_threshold: u32,
    /// Minimum bench cooldown.
    pub min_bench_duration: Duration,
    /// Maximum bench cooldown (the actual duration is drawn uniformly in
    /// `[min, max)`; Go randomizes to avoid thundering-herd unbench).
    pub max_bench_duration: Duration,
}

impl Default for BenchlistConfig {
    fn default() -> Self {
        // Mirrors the spirit of Go's defaults (DefaultBenchDuration = 5m).
        Self {
            failure_threshold: 10,
            min_bench_duration: Duration::from_secs(150),
            max_bench_duration: Duration::from_secs(300),
        }
    }
}

struct PeerState {
    consecutive_failures: u32,
    /// `Some(until)` while benched; cleared once the cooldown elapses.
    benched_until: Option<SystemTime>,
}

struct Inner {
    peers: HashMap<NodeId, PeerState>,
    rng: Mt19937_64,
}

/// Per-chain failure tracker + bench set.
pub struct Benchlist {
    config: BenchlistConfig,
    inner: Mutex<Inner>,
}

impl Benchlist {
    /// Build a benchlist with the given config, seeding the cooldown RNG.
    #[must_use]
    pub fn new(config: BenchlistConfig, seed: u64) -> Self {
        let mut rng = Mt19937_64::new();
        rng.seed(seed);
        Self {
            config,
            inner: Mutex::new(Inner {
                peers: HashMap::new(),
                rng,
            }),
        }
    }

    /// Record a successful response from `node`, resetting its failure count.
    pub fn register_response(&self, node: NodeId) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(p) = inner.peers.get_mut(&node) {
            p.consecutive_failures = 0;
        }
    }

    /// Record a failed request to `node` at `now`. Returns `true` if the peer is
    /// now (newly or already) benched.
    pub fn register_failure(&self, node: NodeId, now: SystemTime) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };

        // Compute the cooldown first to avoid borrowing `inner` mutably twice.
        let cooldown = self.random_cooldown(&mut inner.rng);

        let threshold = self.config.failure_threshold;
        let p = inner.peers.entry(node).or_insert(PeerState {
            consecutive_failures: 0,
            benched_until: None,
        });

        if p.benched_until.is_some() {
            return true;
        }
        p.consecutive_failures = p.consecutive_failures.saturating_add(1);
        if p.consecutive_failures >= threshold {
            p.benched_until = Some(now + cooldown);
            return true;
        }
        false
    }

    /// Whether `node` is currently benched at `now`. Expired benches are cleared
    /// lazily on read (the peer gets another chance).
    pub fn is_benched(&self, node: NodeId, now: SystemTime) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if let Some(p) = inner.peers.get_mut(&node) {
            match p.benched_until {
                Some(until) if now >= until => {
                    p.benched_until = None;
                    p.consecutive_failures = 0;
                    false
                }
                Some(_) => true,
                None => false,
            }
        } else {
            false
        }
    }

    /// Draw a cooldown uniformly in `[min, max)` nanoseconds.
    fn random_cooldown(&self, rng: &mut Mt19937_64) -> Duration {
        let min = self.config.min_bench_duration.as_nanos();
        let max = self.config.max_bench_duration.as_nanos();
        if max <= min {
            return self.config.min_bench_duration;
        }
        let span = max - min;
        // span fits in u128; reduce the 64-bit draw into the span.
        let draw = u128::from(rng.uint64());
        let offset = draw % span;
        let nanos = min.saturating_add(offset);
        // Durations are bounded by config (≤ a few minutes) → fits in u64 nanos.
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
    }

    #[test]
    fn benches_after_threshold_and_cooldown_expires() {
        let cfg = BenchlistConfig {
            failure_threshold: 3,
            min_bench_duration: Duration::from_secs(10),
            max_bench_duration: Duration::from_secs(20),
        };
        let bl = Benchlist::new(cfg, 42);
        let node = NodeId::from([1u8; 20]);
        let t0 = now0();

        assert!(!bl.register_failure(node, t0));
        assert!(!bl.register_failure(node, t0));
        // Third consecutive failure crosses the threshold → benched.
        assert!(bl.register_failure(node, t0));
        assert!(bl.is_benched(node, t0));

        // Within the [10s,20s) cooldown it stays benched.
        assert!(bl.is_benched(node, t0 + Duration::from_secs(5)));
        // After the maximum cooldown it is unbenched (gets another chance).
        assert!(!bl.is_benched(node, t0 + Duration::from_secs(25)));
    }

    #[test]
    fn success_resets_failure_count() {
        let cfg = BenchlistConfig {
            failure_threshold: 2,
            min_bench_duration: Duration::from_secs(10),
            max_bench_duration: Duration::from_secs(20),
        };
        let bl = Benchlist::new(cfg, 7);
        let node = NodeId::from([2u8; 20]);
        let t0 = now0();

        assert!(!bl.register_failure(node, t0));
        bl.register_response(node); // reset
        assert!(!bl.register_failure(node, t0), "count reset, one failure < threshold");
        assert!(!bl.is_benched(node, t0));
    }
}
