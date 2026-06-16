// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Deterministic, seed-derived load-stream generator + rate pacing (M9.18,
//! specs/02 §10.3).
//!
//! The sustained-load arm issues a mixed C-Chain transfer + X/P tx stream at a
//! target rate for a fixed duration. This module is the *pure* heart of that:
//!
//! * [`LoadGenerator`] turns a `(seed, accounts)` pair into a reproducible,
//!   infinite stream of [`TxDescriptor`]s — same seed ⇒ identical descriptor
//!   bytes, distinct seeds ⇒ a different stream. No RNG crate (splitmix64), no
//!   floats, no `unwrap` in library code.
//! * [`PacingSchedule`] is the integer rate math: for a `(rate_per_sec,
//!   duration)` target it computes how many descriptors to emit and at what
//!   spacing, using only checked/saturating arithmetic so a hostile `(rate,
//!   duration)` saturates instead of panicking.
//!
//! The live arm drives a [`LoadGenerator`] for the wall clock, signs+issues each
//! descriptor against a real node, then scrapes `/ext/metrics` and checks the
//! SLOs ([`crate::metrics`]).

use std::time::Duration;

/// Which chain a generated transaction targets. The sustained stream is a mix
/// of C-Chain EVM transfers and X/P-Chain transfers (specs/02 §10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TxKind {
    /// A C-Chain (EVM) value transfer.
    C,
    /// An X-Chain (AVM) asset transfer.
    X,
    /// A P-Chain value transfer (`platformvm` base tx).
    P,
}

impl TxKind {
    /// The stream's round-robin shape: 2 C-Chain transfers for every X and P
    /// transfer (C-Chain throughput dominates the load profile, specs/16 §5).
    ///
    /// Index `i` of the stream maps to `CYCLE[i % CYCLE.len()]`, so the kind
    /// sequence is itself deterministic and seed-independent (only the *amounts
    /// and accounts* vary with the seed).
    const CYCLE: &'static [TxKind] = &[TxKind::C, TxKind::X, TxKind::C, TxKind::P];

    /// The deterministic kind at absolute stream position `index` (cycles over
    /// [`TxKind::CYCLE`]). Index-safe (no panic) via modulo + `get`.
    #[must_use]
    fn at_index(index: u64) -> TxKind {
        let len = u64::try_from(Self::CYCLE.len()).unwrap_or(1).max(1);
        let pos = usize::try_from(index.checked_rem(len).unwrap_or(0)).unwrap_or(0);
        Self::CYCLE.get(pos).copied().unwrap_or(TxKind::C)
    }

    /// A stable 1-byte tag used in the descriptor encoding (byte-exact, so the
    /// determinism gate compares encoded bytes).
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            TxKind::C => 0x01,
            TxKind::X => 0x02,
            TxKind::P => 0x03,
        }
    }
}

/// A single deterministic, seed-derived transaction the generator wants issued.
///
/// This is an *intent* — chain, sender/recipient account indices (into the
/// tmpnet pre-funded key set), and an amount — not a signed tx. The live arm
/// turns it into a signed/issued tx via `ava-wallet`; the offline arm only
/// asserts the encoding is a reproducible function of the seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxDescriptor {
    /// Monotonic position of this descriptor in the stream (from 0).
    pub index: u64,
    /// Which chain the transfer targets.
    pub kind: TxKind,
    /// Index of the sending account within the pre-funded key set.
    pub from: u32,
    /// Index of the receiving account within the pre-funded key set.
    pub to: u32,
    /// Transfer amount in the chain's base denomination (nAVAX / wei-scaled).
    pub amount: u64,
    /// A per-tx nonce/salt so repeated `(from, to, amount)` triples stay
    /// distinct on the wire (e.g. the C-Chain account nonce).
    pub nonce: u64,
}

impl TxDescriptor {
    /// Byte-exact, stable encoding of the descriptor.
    ///
    /// Big-endian, fixed-width fields with a 1-byte kind tag — no `serde`, no
    /// platform-dependent layout — so the determinism gate can compare raw
    /// bytes across runs and machines.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + 8 + 4 + 4 + 8 + 8);
        out.push(self.kind.tag());
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.from.to_be_bytes());
        out.extend_from_slice(&self.to.to_be_bytes());
        out.extend_from_slice(&self.amount.to_be_bytes());
        out.extend_from_slice(&self.nonce.to_be_bytes());
        out
    }
}

/// A deterministic, infinite stream of [`TxDescriptor`]s derived from a seed.
///
/// Reproducible from `(seed, accounts)` alone: the same configuration yields the
/// same descriptor bytes in the same order, while any change to the seed
/// produces a different stream. Uses a splitmix64 step over the descriptor index
/// — no external RNG crate, no floats.
#[derive(Debug, Clone)]
pub struct LoadGenerator {
    seed: u64,
    accounts: u32,
    next_index: u64,
}

impl LoadGenerator {
    /// Build a generator over `accounts` pre-funded accounts, driven by `seed`.
    ///
    /// `accounts` is clamped to at least 2 so a transfer always has a distinct
    /// sender and recipient.
    #[must_use]
    pub fn new(seed: u64, accounts: u32) -> LoadGenerator {
        LoadGenerator {
            seed,
            accounts: accounts.max(2),
            next_index: 0,
        }
    }

    /// The seed this generator was constructed from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The number of pre-funded accounts the stream cycles over (≥ 2).
    #[must_use]
    pub fn accounts(&self) -> u32 {
        self.accounts
    }

    /// Produce the descriptor at absolute stream position `index` without
    /// advancing the cursor — a pure function of `(seed, accounts, index)`.
    #[must_use]
    pub fn descriptor_at(&self, index: u64) -> TxDescriptor {
        // Mix the seed with the index so each position is independently
        // determined (and the whole stream shifts when the seed changes).
        let h = splitmix64(self.seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let h2 = splitmix64(h);

        let kind = TxKind::at_index(index);

        // Derive distinct from/to account indices in `0..accounts`.
        let accounts = u64::from(self.accounts);
        let from_u64 = h.checked_rem(accounts).unwrap_or(0);
        // `to` is offset so it is never equal to `from` (offset in 1..accounts).
        let span = accounts.saturating_sub(1).max(1);
        let offset = h2.checked_rem(span).unwrap_or(0).saturating_add(1);
        let to_u64 = from_u64
            .saturating_add(offset)
            .checked_rem(accounts)
            .unwrap_or(0);
        let from = u32::try_from(from_u64).unwrap_or(0);
        let to = u32::try_from(to_u64).unwrap_or(0);

        // Amount in a bounded band so the funded keys never drain; nonce is the
        // index so repeats stay distinct on the wire.
        let amount = h2.checked_rem(9_000).unwrap_or(0).saturating_add(1_000);

        TxDescriptor {
            index,
            kind,
            from,
            to,
            amount,
            nonce: index,
        }
    }

    /// Pull the next descriptor in the stream, advancing the cursor.
    pub fn next_descriptor(&mut self) -> TxDescriptor {
        let d = self.descriptor_at(self.next_index);
        self.next_index = self.next_index.saturating_add(1);
        d
    }

    /// Materialize the first `count` descriptors (cursor-advancing).
    pub fn take(&mut self, count: u64) -> Vec<TxDescriptor> {
        // `count` is a test/driver-supplied small bound; cap the allocation.
        let cap = usize::try_from(count).unwrap_or(usize::MAX);
        let mut out = Vec::with_capacity(cap.min(1 << 20));
        for _ in 0..count {
            out.push(self.next_descriptor());
        }
        out
    }
}

/// The integer rate-pacing plan for a sustained run: how many descriptors to
/// emit over a duration at a target rate, and the spacing between them.
///
/// All arithmetic is checked/saturating (no floats, no panics) so a hostile
/// `(rate, duration)` can never overflow or divide-by-zero — it degenerates to
/// an empty or single-shot plan instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacingSchedule {
    rate_per_sec: u32,
    duration: Duration,
}

impl PacingSchedule {
    /// A schedule emitting `rate_per_sec` descriptors every second for
    /// `duration`.
    #[must_use]
    pub fn new(rate_per_sec: u32, duration: Duration) -> PacingSchedule {
        PacingSchedule {
            rate_per_sec,
            duration,
        }
    }

    /// The configured target rate (tx/s).
    #[must_use]
    pub fn rate_per_sec(&self) -> u32 {
        self.rate_per_sec
    }

    /// The configured run duration.
    #[must_use]
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Total descriptors to emit over the whole run = `rate * duration`,
    /// computed in integer milliseconds with checked arithmetic.
    ///
    /// `count = floor(rate_per_sec * duration_ms / 1000)`.
    #[must_use]
    pub fn total_count(&self) -> u64 {
        let dur_ms = u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX);
        u64::from(self.rate_per_sec)
            .checked_mul(dur_ms)
            .map_or(u64::MAX, |product| product.checked_div(1_000).unwrap_or(0))
    }

    /// The target spacing between consecutive descriptors. `None` if the rate is
    /// zero (no pacing — the stream is idle).
    #[must_use]
    pub fn interval(&self) -> Option<Duration> {
        if self.rate_per_sec == 0 {
            return None;
        }
        // 1s / rate, in nanoseconds, checked (rate is non-zero here).
        let nanos = 1_000_000_000u64
            .checked_div(u64::from(self.rate_per_sec))
            .unwrap_or(0);
        Some(Duration::from_nanos(nanos))
    }

    /// The deadline offset of descriptor `i` from the run start: `i * interval`,
    /// saturating at the run `duration`. Used by the live arm to pace issuance.
    #[must_use]
    pub fn deadline_of(&self, i: u64) -> Duration {
        let Some(interval) = self.interval() else {
            return Duration::ZERO;
        };
        let nanos = interval
            .as_nanos()
            .saturating_mul(u128::from(i))
            .min(self.duration.as_nanos());
        let nanos = u64::try_from(nanos).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos)
    }
}

/// A single splitmix64 finalization step (deterministic, no external RNG dep).
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}
