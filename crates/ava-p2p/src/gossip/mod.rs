// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Gossip framework traits (Go `network/p2p/gossip/{gossip.go,set.go}`).
//!
//! This module carries the shared vocabulary — [`Gossipable`],
//! [`Marshaller`], and [`Set`] — the concrete [`bloom::BloomSet`] filter used
//! to answer "have I already seen this id" without shipping the full working
//! set over the wire, the shared tunables ([`GossipParams`]) and driver loop
//! ([`every`]), and the push/pull gossip drivers themselves
//! ([`push::PushGossiper`], [`pull::PullGossiper`]) plus the inbound
//! [`handler::GossipHandler`]. A concrete `Set` implementation (backed by the
//! C-Chain tx pool) lands in a later task.
//!
//! Trait signatures are written to stay object-safe (`Box<dyn Set<T>>`) —
//! see each trait's doc for the Go method it mirrors.

pub mod bloom;
pub mod handler;
pub mod pull;
pub mod push;

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use ava_types::id::Id;
use ava_vm::app_sender::SendConfig;

use crate::error::Result;

/// An item that can be gossiped across the network (Go `gossip.Gossipable`).
pub trait Gossipable: Send + Sync {
    /// Returns the id used to deduplicate this item across gossip rounds
    /// (Go `Gossipable.GossipID`).
    fn gossip_id(&self) -> Id;
}

/// Parsing logic for a concrete [`Gossipable`] type (Go `gossip.Marshaller`).
pub trait Marshaller<T>: Send + Sync {
    /// Serializes `t` to its wire representation (Go `MarshalGossip`).
    ///
    /// # Errors
    /// Returns an error if `t` could not be serialized.
    fn marshal(&self, t: &T) -> Result<Vec<u8>>;

    /// Deserializes `bytes` into a `T` (Go `UnmarshalGossip`).
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid encoding of `T`.
    fn unmarshal(&self, bytes: &[u8]) -> Result<T>;
}

/// A set of known gossipable items that also exposes a compact bloom-filter
/// summary of its contents (Go `gossip.Set`, which embeds `HandlerSet` +
/// `PushGossiperSet` + `Len`).
///
/// Methods take `&self` (not `&mut self`) so implementations must use
/// interior mutability — matching Go's `*BloomSet[T]`/mempool-backed sets,
/// which guard their state behind a `sync.RWMutex` and are shared via a
/// plain pointer.
pub trait Set<T: Gossipable>: Send + Sync {
    /// Adds `t` to the set.
    ///
    /// # Errors
    /// Returns an error if `t` was not added (Go `Add(v T) error`).
    fn add(&self, t: T) -> Result<()>;

    /// Returns whether `id` is a known member of the set (Go
    /// `PushGossiperSet.Has`).
    fn has(&self, id: &Id) -> bool;

    /// Iterates every item currently tracked, in implementation-defined
    /// order, stopping early if `f` returns `false` (Go `Iterate(f func(T)
    /// bool)`).
    fn iterate(&self, f: &mut dyn FnMut(&T) -> bool);

    /// Returns `(bloom_marshal_bytes, salt)` for the set's current bloom
    /// filter (Go `BloomFilter() (*bloom.Filter, ids.ID)`, wire-encoded via
    /// `bloom.Filter.Marshal`/`ids.ID` bytes so it can be shipped to peers
    /// and parsed by [`ava_utils::bloom::ReadFilter`] on either side).
    fn get_filter(&self) -> (Vec<u8>, Vec<u8>);
}

/// Shared tunables for a gossip system (Go `gossip.SystemConfig` +
/// `setDefaults`, `network/p2p/gossip/system.go:25-77`; the frequency/count
/// defaults below are the *concrete* values coreth's C-Chain VM plugs into
/// that config, `graft/coreth/plugin/evm/config/default_config.go:54-61`
/// (`PushGossipFrequency`/`PullGossipFrequency`/`RegossipFrequency`/
/// `PushGossipNumValidators`/`PushRegossipNumValidators`), since
/// `SystemConfig` itself has no compile-time constant defaults for those
/// fields other than what `setDefaults` inlines — this port takes the
/// coreth numbers directly as `Default`).
///
/// **Simplification (pre-authorized by the task-6 brief): push targeting.**
/// Go's `BranchingFactor` (`gossip.go:395-410`) has four knobs —
/// `StakePercentage` (inverse-CDF stake sampling), `Validators`,
/// `NonValidators`, and `Peers` (flat connected-node counts) — and coreth's
/// defaults set `PushGossipParams = {StakePercentage: .9, Validators: 100}`
/// / `PushRegossipParams = {Validators: 10}` (`system.go:60-69`). This port
/// has no validator-stake sampler, so [`GossipParams::push_cfg`]/
/// [`GossipParams::regossip_cfg`] carry only the `validators` count (100 /
/// 10) via [`ava_vm::app_sender::SendConfig`] — the `StakePercentage: .9`
/// half of Go's default has no analog here; the production `AppSender`
/// (`OutboundSender`) resolves `SendConfig`'s node sampling.
#[derive(Clone, Debug)]
pub struct GossipParams {
    /// Soft cap, in bytes, on a single `PushGossip`/`PullGossipResponse`
    /// batch (Go `SystemConfig.TargetMessageSize`, defaults to 20 KiB,
    /// `system.go:32,51-53`).
    pub target_message_size: usize,
    /// How often a push-gossip cycle runs (Go coreth
    /// `PushGossipFrequency`, `default_config.go:59`; `gossip.SystemConfig`
    /// has no separate push-cadence field of its own — cadence is supplied
    /// by whatever drives `gossip.Every`, which is this field here).
    pub push_period: Duration,
    /// How often a pull-gossip cycle runs (Go `SystemConfig.RequestPeriod`,
    /// defaults to one second, `system.go:35,57-59`, matching coreth's
    /// `PullGossipFrequency`, `default_config.go:60`).
    pub pull_period: Duration,
    /// How often the regossip queue is re-sent (Go
    /// `SystemConfig.RegossipPeriod`, defaults to 30 seconds,
    /// `system.go:41,74-76`, matching coreth's `RegossipFrequency`,
    /// `default_config.go:61`).
    pub regossip_period: Duration,
    /// Push-gossip targeting (Go `SystemConfig.PushGossipParams`, defaults
    /// to `{StakePercentage: .9, Validators: 100}`, `system.go:37,60-65`) —
    /// narrowed to `validators: 100` only; see the simplification note above.
    pub push_cfg: SendConfig,
    /// Regossip targeting (Go `SystemConfig.PushRegossipParams`, defaults to
    /// `{Validators: 10}`, `system.go:38,66-69`) — `validators: 10`; see the
    /// simplification note above.
    pub regossip_cfg: SendConfig,
    /// Capacity of the "recently discarded" cache (Go
    /// `SystemConfig.DiscardedPushCacheSize`, defaults to 16,384,
    /// `system.go:40,71-73`).
    pub discarded_cache_size: usize,
}

impl Default for GossipParams {
    fn default() -> Self {
        Self {
            // Go `units.KiB` = 1024 (`system.go:52`, `20 * units.KiB`).
            target_message_size: 20 * 1024,
            push_period: Duration::from_millis(100),
            pull_period: Duration::from_secs(1),
            regossip_period: Duration::from_secs(30),
            push_cfg: SendConfig {
                validators: 100,
                ..SendConfig::default()
            },
            regossip_cfg: SendConfig {
                validators: 10,
                ..SendConfig::default()
            },
            discarded_cache_size: 16_384,
        }
    }
}

/// Runs `cycle` once per `period` until `token` is cancelled (Go
/// `gossip.Every`, `network/p2p/gossip/gossip.go:614-634`).
///
/// A `cycle` that returns `Err` is logged (`tracing::warn!`) and swallowed —
/// one failed cycle does not stop the loop — matching Go's
/// `if err := gossiper.Gossip(ctx); err != nil { log.Warn(...) }`.
///
/// Uses [`tokio::time::interval`] with
/// [`tokio::time::MissedTickBehavior::Skip`], the closest analog to Go's
/// `time.Ticker`: Go's ticker channel has a buffer of exactly one, so ticks
/// that arrive while `cycle` (the previous `Gossip` call) is still running
/// are silently dropped rather than queued — `Skip` likewise drops missed
/// ticks and resumes on the original period-aligned schedule, unlike tokio's
/// default `Burst` (fire once per missed tick, back-to-back) or `Delay`
/// (shift the whole schedule out by the overrun).
///
/// Driving this off [`tokio::time::interval`] (rather than
/// `tokio::time::sleep` in a loop) is what makes the loop honor
/// `#[tokio::test(start_paused = true)]` virtual time in tests: a paused
/// clock auto-advances to the next pending timer once every task is parked
/// waiting on one.
pub async fn every<F, Fut>(token: CancellationToken, period: Duration, mut cycle: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = cycle().await {
                    tracing::warn!(error = %err, "failed to gossip");
                }
            }
            () = token.cancelled() => {
                tracing::debug!("shutting down gossip");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A minimal `Gossipable` used only to exercise object-safety below.
    struct DummyGossipable(Id);

    impl Gossipable for DummyGossipable {
        fn gossip_id(&self) -> Id {
            self.0
        }
    }

    /// A trivial in-memory `Set` impl — exists only to prove [`Set`] is
    /// object-safe (`Box<dyn Set<T>>`), per this module's design note.
    struct DummySet {
        items: Mutex<Vec<DummyGossipable>>,
    }

    impl Set<DummyGossipable> for DummySet {
        fn add(&self, t: DummyGossipable) -> Result<()> {
            self.items.lock().unwrap_or_else(|e| e.into_inner()).push(t);
            Ok(())
        }

        fn has(&self, id: &Id) -> bool {
            self.items
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|g| &g.0 == id)
        }

        fn iterate(&self, f: &mut dyn FnMut(&DummyGossipable) -> bool) {
            for item in self.items.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if !f(item) {
                    break;
                }
            }
        }

        fn get_filter(&self) -> (Vec<u8>, Vec<u8>) {
            (Vec::new(), Vec::new())
        }
    }

    #[test]
    fn set_trait_is_object_safe_and_dispatches() {
        let set: Box<dyn Set<DummyGossipable>> = Box::new(DummySet {
            items: Mutex::new(Vec::new()),
        });
        let id = Id::from([1u8; 32]);
        set.add(DummyGossipable(id)).expect("DummySet::add");
        assert!(set.has(&id), "added id should be found via has()");

        let mut seen = 0usize;
        set.iterate(&mut |_| {
            seen += 1;
            true
        });
        assert_eq!(seen, 1, "iterate should visit the one added item");
    }
}
