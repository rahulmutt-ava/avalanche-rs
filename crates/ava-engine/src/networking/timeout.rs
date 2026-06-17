// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Adaptive timeout manager (port of `utils/timer/adaptive_timeout_manager.go`
//! + `utils/math/continuous_averager.go`, specs 06 §5.4).
//!
//! Tracks an exponentially-decaying average of observed network latency and
//! sets the per-request timeout to `timeout_coefficient × average`, clamped to
//! `[minimum_timeout, maximum_timeout]`. The averager is **float** math, which
//! is acceptable here because timeouts affect only liveness/latency, never which
//! block is accepted (off the consensus-determinism path; specs 24 §B.4).
//!
//! Timers fire over `tokio::time` so the manager honors `start_paused` +
//! `tokio::time::advance` in virtual-time tests (specs 24 §B.2). All elapsed-time
//! reads go through `clock.monotonic()`.
//!
//! The float math in this module is the exponentially-decaying latency averager
//! only; it feeds the per-request timeout (liveness/latency), never a
//! block/vote/decision (spec 24 §B.3/§B.4 / hazard #2), so `float_arithmetic` is
//! allowed module-wide here.
#![allow(clippy::float_arithmetic)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::clock::Clock;

/// Errors from configuring or running the adaptive timeout manager.
#[derive(Debug, thiserror::Error)]
pub enum TimeoutError {
    /// `initial_timeout > maximum_timeout`.
    #[error("initial timeout ({initial:?}) cannot be greater than maximum timeout ({maximum:?})")]
    InitialAboveMaximum {
        /// The configured initial timeout.
        initial: Duration,
        /// The configured maximum timeout.
        maximum: Duration,
    },
    /// `initial_timeout < minimum_timeout`.
    #[error("initial timeout ({initial:?}) cannot be less than minimum timeout ({minimum:?})")]
    InitialBelowMinimum {
        /// The configured initial timeout.
        initial: Duration,
        /// The configured minimum timeout.
        minimum: Duration,
    },
    /// `timeout_coefficient < 1`.
    #[error("timeout coefficient ({coefficient}) must be >= 1")]
    CoefficientTooSmall {
        /// The configured coefficient.
        coefficient: f64,
    },
    /// `timeout_halflife == 0`.
    #[error("timeout halflife must be positive")]
    NonPositiveHalflife,
}

/// The opaque key identifying an outstanding request (Go `ids.RequestID`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId {
    /// The peer the request was sent to.
    pub node: NodeId,
    /// The chain the request belongs to.
    pub chain: Id,
    /// The wire request ID.
    pub request_id: u32,
    /// The op (numeric tag) of the request, so the failure op can be synthesized.
    pub op: u8,
}

/// `utils/timer.AdaptiveTimeoutConfig` — the adaptive timeout parameters.
#[derive(Clone, Debug)]
pub struct AdaptiveTimeoutConfig {
    /// The starting timeout, used before any latency is observed.
    pub initial_timeout: Duration,
    /// The lower clamp on the computed timeout.
    pub minimum_timeout: Duration,
    /// The upper clamp on the computed timeout.
    pub maximum_timeout: Duration,
    /// `timeout = timeout_coefficient × average response time`; must be `>= 1`.
    pub timeout_coefficient: f64,
    /// Larger halflife → less volatile timeout; must be positive.
    pub timeout_halflife: Duration,
}

impl AdaptiveTimeoutConfig {
    /// Validate the config in Go's exact branch order.
    ///
    /// # Errors
    /// Returns [`TimeoutError`] for an out-of-range initial timeout, a
    /// coefficient `< 1`, or a non-positive halflife.
    pub fn verify(&self) -> Result<(), TimeoutError> {
        if self.initial_timeout > self.maximum_timeout {
            return Err(TimeoutError::InitialAboveMaximum {
                initial: self.initial_timeout,
                maximum: self.maximum_timeout,
            });
        }
        if self.initial_timeout < self.minimum_timeout {
            return Err(TimeoutError::InitialBelowMinimum {
                initial: self.initial_timeout,
                minimum: self.minimum_timeout,
            });
        }
        if self.timeout_coefficient < 1.0 {
            return Err(TimeoutError::CoefficientTooSmall {
                coefficient: self.timeout_coefficient,
            });
        }
        if self.timeout_halflife.is_zero() {
            return Err(TimeoutError::NonPositiveHalflife);
        }
        Ok(())
    }
}

/// Continuous-time exponential moving average (port of `continuousAverager`).
///
/// Float math; off the consensus-determinism path. Time is measured in
/// nanoseconds relative to an [`Instant`] origin so the half-life decay matches
/// Go's `float64(time.Duration)` arithmetic.
struct ContinuousAverager {
    /// `halflife / ln2`, in nanoseconds.
    halflife: f64,
    weighted_sum: f64,
    normalizer: f64,
    last_updated: Instant,
}

impl ContinuousAverager {
    fn new(initial_prediction: f64, halflife: Duration, now: Instant) -> Self {
        Self {
            halflife: halflife.as_nanos() as f64 / std::f64::consts::LN_2,
            weighted_sum: initial_prediction,
            normalizer: 1.0,
            last_updated: now,
        }
    }

    /// Observe `value` at `now`. Mirrors Go's three-branch ordering on the sign
    /// of `last_updated - now`.
    fn observe(&mut self, value: f64, now: Instant) {
        if now > self.last_updated {
            // Times in order: scale previous values to keep sizes manageable.
            let delta = now.duration_since(self.last_updated).as_nanos() as f64;
            let new_weight = (-delta / self.halflife).exp();
            self.weighted_sum = value + new_weight * self.weighted_sum;
            self.normalizer = 1.0 + new_weight * self.normalizer;
            self.last_updated = now;
        } else if now == self.last_updated {
            self.weighted_sum += value;
            self.normalizer += 1.0;
        } else {
            // Out of order: don't scale previous values.
            let delta = self.last_updated.duration_since(now).as_nanos() as f64;
            let new_weight = (-delta / self.halflife).exp();
            self.weighted_sum += new_weight * value;
            self.normalizer += new_weight;
        }
    }

    fn read(&self) -> f64 {
        self.weighted_sum / self.normalizer
    }
}

/// The handler run when a request times out.
pub type TimeoutHandler = Box<dyn FnOnce() + Send + 'static>;

struct PendingTimeout {
    deadline: Instant,
    duration: Duration,
    measure_latency: bool,
    handler: TimeoutHandler,
}

struct ManagerState {
    averager: ContinuousAverager,
    timeout_coefficient: f64,
    minimum_timeout: Duration,
    maximum_timeout: Duration,
    current_timeout: Duration,
    pending: HashMap<RequestId, PendingTimeout>,
}

impl ManagerState {
    /// Records an observed latency and recomputes `current_timeout`
    /// (clamped to `[min, max]`). Mirrors Go `observeLatencyAndUpdateTimeout`.
    fn observe_latency(&mut self, latency: Duration, now: Instant) {
        self.averager.observe(latency.as_nanos() as f64, now);
        let avg = self.averager.read();
        let nanos = self.timeout_coefficient * avg;
        let nanos = if nanos.is_finite() && nanos >= 0.0 {
            nanos
        } else {
            0.0
        };
        // `nanos` is finite and >= 0 here; Rust's float→int cast saturates, so an
        // out-of-range value clamps to u64::MAX (then re-clamped to maximum_timeout
        // below) rather than wrapping — matching Go's overflow-to-max behavior.
        #[allow(clippy::cast_possible_truncation)]
        // justification: saturating float→int cast of a clamped, finite value
        let nanos = nanos as u64;
        let mut timeout = Duration::from_nanos(nanos);
        if timeout > self.maximum_timeout {
            timeout = self.maximum_timeout;
        } else if timeout < self.minimum_timeout {
            timeout = self.minimum_timeout;
        }
        self.current_timeout = timeout;
    }
}

/// `AdaptiveTimeoutManager` — the per-network request timeout registry.
///
/// One background tokio task watches for the earliest deadline and fires expired
/// handlers; `put`/`remove`/`observe_latency` adjust the averaged timeout.
pub struct AdaptiveTimeoutManager {
    state: Arc<Mutex<ManagerState>>,
    clock: Arc<dyn Clock>,
    /// Wakes the dispatch loop after a `put`/`remove` so it recomputes the next
    /// deadline.
    wake: mpsc::UnboundedSender<()>,
    shutdown: CancellationToken,
}

impl AdaptiveTimeoutManager {
    /// Build a manager from a verified config. Spawns the dispatch task.
    ///
    /// # Errors
    /// Returns [`TimeoutError`] if the config fails [`AdaptiveTimeoutConfig::verify`].
    pub fn new(
        config: &AdaptiveTimeoutConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, TimeoutError> {
        config.verify()?;

        let now = clock.monotonic();
        let state = Arc::new(Mutex::new(ManagerState {
            averager: ContinuousAverager::new(
                config.initial_timeout.as_nanos() as f64,
                config.timeout_halflife,
                now,
            ),
            timeout_coefficient: config.timeout_coefficient,
            minimum_timeout: config.minimum_timeout,
            maximum_timeout: config.maximum_timeout,
            current_timeout: config.initial_timeout,
            pending: HashMap::new(),
        }));

        let (wake, wake_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();

        let mgr = Self {
            state: state.clone(),
            clock: clock.clone(),
            wake,
            shutdown: shutdown.clone(),
        };
        tokio::spawn(dispatch_loop(state, clock, wake_rx, shutdown));
        Ok(mgr)
    }

    /// Returns the current network timeout duration.
    pub async fn timeout_duration(&self) -> Duration {
        self.state.lock().await.current_timeout
    }

    /// Register a timeout for `id`. If it is not [`remove`](Self::remove)d before
    /// the deadline, `handler` is invoked. `measure_latency` controls whether a
    /// response/timeout for this request feeds the latency average.
    pub async fn put(&self, id: RequestId, measure_latency: bool, handler: TimeoutHandler) {
        let now = self.clock.monotonic();
        {
            let mut st = self.state.lock().await;
            // Replace any existing timeout with the same id (no latency observed).
            st.pending.remove(&id);
            let duration = st.current_timeout;
            st.pending.insert(
                id,
                PendingTimeout {
                    // Saturate rather than overflow the monotonic deadline; a
                    // saturated deadline simply fires later (liveness-only).
                    deadline: now.checked_add(duration).unwrap_or(now),
                    duration,
                    measure_latency,
                    handler,
                },
            );
        }
        let _ = self.wake.send(());
    }

    /// Remove the timeout associated with `id`; its handler will not fire. If the
    /// request measured latency, the observed response time updates the average.
    pub async fn remove(&self, id: RequestId) {
        let now = self.clock.monotonic();
        {
            let mut st = self.state.lock().await;
            if let Some(t) = st.pending.remove(&id)
                && t.measure_latency
            {
                let registered_at = t.deadline.checked_sub(t.duration).unwrap_or(t.deadline);
                let latency = now.saturating_duration_since(registered_at);
                st.observe_latency(latency, now);
            }
        }
        let _ = self.wake.send(());
    }

    /// Manually register a response latency (e.g. to pretend a benched validator
    /// timed out without sending it a request). Mirrors Go `ObserveLatency`.
    pub async fn observe_latency(&self, latency: Duration) {
        let now = self.clock.monotonic();
        self.state.lock().await.observe_latency(latency, now);
    }

    /// Stop the dispatch task. Idempotent.
    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for AdaptiveTimeoutManager {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// The background loop: sleeps until the earliest deadline, fires expired
/// handlers (observing their full duration as latency), and recomputes.
async fn dispatch_loop(
    state: Arc<Mutex<ManagerState>>,
    clock: Arc<dyn Clock>,
    mut wake_rx: mpsc::UnboundedReceiver<()>,
    shutdown: CancellationToken,
) {
    loop {
        // Compute the next deadline.
        let next = {
            let st = state.lock().await;
            st.pending.values().map(|t| t.deadline).min()
        };

        let sleep_fut = async {
            match next {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                // No pending timeouts: park until woken.
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            _ = wake_rx.recv() => {
                // A put/remove changed the set; recompute the deadline.
                continue;
            }
            () = sleep_fut => {
                fire_expired(&state, &clock).await;
            }
        }
    }
}

/// Pop and fire every timeout whose deadline is at or before `now`.
async fn fire_expired(state: &Arc<Mutex<ManagerState>>, clock: &Arc<dyn Clock>) {
    loop {
        let now = clock.monotonic();
        let fired = {
            let mut st = state.lock().await;
            // Find the earliest expired entry.
            let expired = st
                .pending
                .iter()
                .filter(|(_, t)| t.deadline <= now)
                .map(|(id, _)| *id)
                .min_by_key(|id| st.pending.get(id).map(|t| t.deadline));
            match expired {
                Some(id) => {
                    let t = st.pending.remove(&id);
                    // A timed-out request observes its full duration as latency.
                    if let Some(t) = &t
                        && t.measure_latency
                    {
                        st.observe_latency(t.duration, now);
                    }
                    t.map(|t| t.handler)
                }
                None => None,
            }
        };
        match fired {
            Some(handler) => handler(),
            None => break,
        }
    }
}
