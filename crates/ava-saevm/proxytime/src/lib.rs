// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-proxytime` — the SAE proxy-unit clock (`specs/11 §2.1`).
//!
//! [`Time<D>`] represents an instant whose passage is measured in a proxy unit
//! `D: ProxyUnit` (typically gas). The rate field `hertz` says how many proxy
//! units equal one wall-clock second. The fractional-second component is always
//! kept in `[0, hertz)`.
//!
//! # Correspondence to the Go reference
//!
//! This crate is a faithful Rust port of
//! `vms/saevm/proxytime/proxytime.go` (`Time[D Duration]`).
//! Notable differences:
//!
//! * Arithmetic is done via `u128` widening rather than `math/bits.Add64` /
//!   `Div64`, satisfying `clippy::arithmetic_side_effects` (denied in the SAE
//!   stricter lint pass).
//! * The serialised form uses a plain big-endian byte encoding for
//!   `(seconds: u64, fraction: u64, hertz: u64)` — exact Go-canoto byte
//!   parity is deferred to M7.8 types which owns the persisted execution blob.
//!
//! # Type-bridging decision for `compare`
//!
//! `D: Into<u128>` (not `D: Into<u64>`), so we **cannot** call
//! [`ava_saevm_cmputils::compare_fractions`] directly (that takes `u64`
//! operands). Instead we replicate the identical algorithm inline in `u128`,
//! which is safe because the cross-products of two values that each originally
//! fit in `u64` always fit in `u128` (`(2^64 − 1)^2 < 2^128`). This mirrors
//! the Go `bits.Mul64` 128-bit cross-multiply exactly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]

use std::cmp::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ava_saevm_intmath::mul_div_ceil;

// ---------------------------------------------------------------------------
// ProxyUnit trait
// ---------------------------------------------------------------------------

/// A proxy-unit type suitable for parameterising [`Time<D>`].
///
/// Implementors must be `Copy + Ord + Into<u128> + From<u64>`. The `From<u64>`
/// bound lets `Time<D>` arithmetic convert `u128` remainders back to `D`
/// without lossy `as` casts (all current instantiations are newtype wrappers
/// over `u64`).
///
/// Mirrors Go's `type Duration interface { ~uint64 }`.
pub trait ProxyUnit: Copy + Ord + Into<u128> + From<u64> {}

// ---------------------------------------------------------------------------
// FractionalSecond
// ---------------------------------------------------------------------------

/// A sub-second duration expressed as a fraction `numerator / denominator`.
///
/// Mirrors `proxytime.go::FractionalSecond[D]`. Both fields are denominated in
/// the same proxy unit `D`. The invariant `numerator < denominator` holds on
/// values returned by [`Time::fraction`], but callers that receive a delta need
/// not enforce that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FractionalSecond<D> {
    /// The number of proxy units representing the sub-second portion.
    pub numerator: D,
    /// The number of proxy units per second (= [`Time::rate`]).
    pub denominator: D,
}

// ---------------------------------------------------------------------------
// Time<D>
// ---------------------------------------------------------------------------

/// An instant in time whose passage is measured in proxy unit `D`.
///
/// `seconds` is the Unix timestamp component; `fraction` is the sub-second
/// portion denominated in `hertz` (proxy units per second). Invariant:
/// `fraction < hertz` (maintained by all mutating methods).
///
/// The zero value is **not** valid — always construct with [`Time::new`].
///
/// Corresponds to Go's `proxytime.Time[D Duration]`.
#[derive(Clone, Debug)]
pub struct Time<D: ProxyUnit> {
    seconds: u64,
    /// Invariant: `fraction < hertz`.
    fraction: D,
    hertz: D,
}

impl<D: ProxyUnit> Time<D> {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Creates a new `Time` at the given Unix `seconds`, with an initial
    /// sub-second `frac` (denominated in `hertz`). If `frac >= hertz` it is
    /// normalised via [`tick`].
    ///
    /// Mirrors `proxytime.go::New[D]`.
    ///
    /// # Panics
    ///
    /// Panics if `hertz == 0` (a zero rate is a programming error, never a
    /// runtime condition the SAE call sites pass).
    #[must_use]
    pub fn new(seconds: u64, frac: D, hertz: D) -> Self {
        let zero: D = D::from(0u64);
        assert!(hertz > zero, "hertz must be > 0");
        let mut t = Self {
            seconds,
            fraction: zero,
            hertz,
        };
        t.tick(frac);
        t
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Returns the Unix timestamp (whole-seconds) component.
    ///
    /// Mirrors `proxytime.go::Unix`.
    #[must_use]
    pub fn unix_seconds(&self) -> u64 {
        self.seconds
    }

    /// Returns the rate: proxy units per wall-clock second.
    ///
    /// Mirrors `proxytime.go::Rate`.
    #[must_use]
    pub fn rate(&self) -> D {
        self.hertz
    }

    /// Returns the sub-second fractional component as a [`FractionalSecond`].
    ///
    /// Mirrors `proxytime.go::Fraction`.
    #[must_use]
    pub fn fraction(&self) -> FractionalSecond<D> {
        FractionalSecond {
            numerator: self.fraction,
            denominator: self.hertz,
        }
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Advances the clock by `d` proxy units.
    ///
    /// Closed-form: `total = fraction + d` (in `u128`); then
    /// `seconds += total / hertz`; `fraction = total % hertz`.
    /// Uses `u128` widening so there is no overflow even when
    /// `fraction + d >= 2^64`. Mirrors `proxytime.go::Tick` (which uses
    /// `bits.Add64` + `bits.Div64` for the same effect).
    ///
    /// # Panics
    ///
    /// Panics if the `hertz > 0` invariant is somehow broken (this is a
    /// programming error, never a runtime condition at valid call sites).
    pub fn tick(&mut self, d: D) {
        let frac_128: u128 = self.fraction.into();
        let d_128: u128 = d.into();
        let hz_128: u128 = self.hertz.into();

        // Both operands fit in u64, so their sum fits in u65 ⊂ u128.
        // wrapping_add is lint-clean; overflow is impossible.
        let total = frac_128.wrapping_add(d_128);
        // hz_128 >= 1 (hertz must be > 0), so division is safe.
        let carry = total
            .checked_div(hz_128)
            .expect("hertz must be > 0 (invariant)");
        let rem = total
            .checked_rem(hz_128)
            .expect("hertz must be > 0 (invariant)");

        // carry ≤ (u64::MAX + u64::MAX) / 1 ≤ 2*u64::MAX which may overflow
        // u64; saturate rather than panic.
        let carry_u64 = u64::try_from(carry).unwrap_or(u64::MAX);
        self.seconds = self.seconds.saturating_add(carry_u64);
        // rem < hertz ≤ u64::MAX so always fits in u64.
        self.fraction = D::from(u64::try_from(rem).unwrap_or(0));
    }

    /// Sets the clock to `(to, to_frac)` if it is strictly in the future,
    /// returning how far the clock advanced as `(whole_seconds,
    /// fractional_second)`. Returns `(0, zero_fraction)` without mutating
    /// `self` if `(to, to_frac)` is not strictly after `self`.
    ///
    /// `to_frac` is first normalised (may be >= hertz) via division:
    /// `to += to_frac / hertz; to_frac %= hertz`. Mirrors
    /// `proxytime.go::FastForwardTo`.
    ///
    /// # Panics
    ///
    /// Panics if the `hertz > 0` invariant is somehow broken (programming error).
    pub fn fast_forward_to(&mut self, to: u64, to_frac: D) -> (u64, FractionalSecond<D>) {
        let frac_128: u128 = to_frac.into();
        let hz_128: u128 = self.hertz.into();

        // Normalise to_frac; hz_128 > 0 guaranteed by invariant.
        let extra_secs = frac_128
            .checked_div(hz_128)
            .expect("hertz must be > 0 (invariant)");
        let rem_frac = frac_128
            .checked_rem(hz_128)
            .expect("hertz must be > 0 (invariant)");

        let extra_secs_u64 = u64::try_from(extra_secs).unwrap_or(u64::MAX);
        let to = to.saturating_add(extra_secs_u64);
        // rem_frac < hertz ≤ u64::MAX, fits.
        let to_frac = D::from(u64::try_from(rem_frac).unwrap_or(0));

        if !self.is_future(to, to_frac) {
            return (
                0,
                FractionalSecond {
                    numerator: D::from(0u64),
                    denominator: self.hertz,
                },
            );
        }

        // Compute the delta using explicit borrow accounting.
        // Since to > self.seconds OR (to == self.seconds AND to_frac > self.fraction),
        // the subtraction is safe provided we handle the fractional borrow.
        let (ff_sec, ff_frac_num) = if self.fraction > to_frac {
            // Borrow one second; numerator = hertz - (fraction - to_frac).
            let sec_diff = to.wrapping_sub(self.seconds).wrapping_sub(1);
            let hz_u: u128 = self.hertz.into();
            let tf: u128 = to_frac.into();
            let sf: u128 = self.fraction.into();
            // hz + to_frac - fraction fits in u128 (all ≤ u64::MAX).
            let num = hz_u.wrapping_add(tf).wrapping_sub(sf);
            let num_d = D::from(u64::try_from(num).unwrap_or(0));
            (sec_diff, num_d)
        } else {
            let sec_diff = to.wrapping_sub(self.seconds);
            let tf: u128 = to_frac.into();
            let sf: u128 = self.fraction.into();
            let num = tf.wrapping_sub(sf);
            let num_d = D::from(u64::try_from(num).unwrap_or(0));
            (sec_diff, num_d)
        };

        self.seconds = to;
        self.fraction = to_frac;

        (
            ff_sec,
            FractionalSecond {
                numerator: ff_frac_num,
                denominator: self.hertz,
            },
        )
    }

    /// Changes the proxy-unit rate, rescaling the fractional second to the new
    /// hertz by rounding **up** (`ceil`) to maintain monotonicity.
    ///
    /// Formula: `new_fraction = ⌈old_fraction · new_hertz / old_hertz⌉`.
    /// Uses [`ava_saevm_intmath::mul_div_ceil`] which computes the product in
    /// `u128` before dividing, avoiding intermediate overflow.
    ///
    /// Mirrors `proxytime.go::SetRate` + `scaleFraction` + `MulDivCeil`.
    ///
    /// # Panics
    ///
    /// Panics if `new_hertz == 0` or if the `fraction < hertz` invariant is
    /// somehow already broken (defensive, matches Go's panic in
    /// `scaleFraction`).
    pub fn set_rate(&mut self, hertz: D) {
        assert!(hertz > D::from(0u64), "hertz must be > 0");

        // Extract as u64 (D: Into<u128> and all practical D are ~u64, so
        // try_from succeeds for valid values; panic if the invariant is broken).
        let old_frac = u64::try_from(self.fraction.into())
            .expect("broken invariant: fraction exceeds u64");
        let old_hz = u64::try_from(self.hertz.into())
            .expect("broken invariant: hertz exceeds u64");
        let new_hz =
            u64::try_from(hertz.into()).expect("new hertz exceeds u64");

        let new_frac = mul_div_ceil(old_frac, new_hz, old_hz)
            .expect("broken invariant: set_rate overflow (fraction < hertz must hold)");

        // Defensive carry: if ceil produced new_frac == new_hz, carry one
        // second. Mirrors Go's `if frac >= hertz { frac -= hertz; tm.seconds++ }`.
        let (new_frac_adj, seconds_bump) = if new_frac >= new_hz {
            (new_frac.wrapping_sub(new_hz), 1u64)
        } else {
            (new_frac, 0u64)
        };

        self.seconds = self.seconds.saturating_add(seconds_bump);
        self.fraction = D::from(new_frac_adj);
        self.hertz = hertz;
    }

    /// Compares `self` with `other`, returning their temporal ordering.
    ///
    /// The two instants may have different rates (`hertz`).
    ///
    /// Algorithm (mirrors `proxytime.go::Compare` + `FractionalSecond.Compare`):
    /// 1. Compare whole `seconds` directly.
    /// 2. If seconds are equal, compare `self.fraction / self.hertz` vs
    ///    `other.fraction / other.hertz` by cross-multiplying in `u128`:
    ///    `self.fraction * other.hertz` vs `other.fraction * self.hertz`.
    ///
    /// # Why inline u128 rather than `cmputils::compare_fractions`
    ///
    /// [`ava_saevm_cmputils::compare_fractions`] takes `u64` operands, but
    /// `D: Into<u128>` (not `Into<u64>`), so calling it would require a
    /// narrowing `try_from` cast that could silently truncate an unusual `D`.
    /// We replicate the identical algorithm directly in `u128`. The result is
    /// the same for all `D` backed by at most 64 bits (which the `D: ~uint64`
    /// constraint guarantees).
    #[must_use]
    pub fn compare(&self, other: &Self) -> Ordering {
        match self.seconds.cmp(&other.seconds) {
            Ordering::Equal => {}
            ord => return ord,
        }
        // Cross-multiply fractions in u128 (exact; no overflow for u64-backed D).
        let lhs: u128 = self.fraction.into();
        let rhs_hz: u128 = other.hertz.into();
        let rhs: u128 = other.fraction.into();
        let lhs_hz: u128 = self.hertz.into();
        lhs.wrapping_mul(rhs_hz).cmp(&rhs.wrapping_mul(lhs_hz))
    }

    /// Converts to a [`SystemTime`] for metrics and logging only.
    ///
    /// Rescales the fractional second to nanosecond precision (hertz = 1e9) by
    /// rounding up, matching `proxytime.go::AsTime`. Not suitable for consensus
    /// logic.
    #[must_use]
    pub fn as_time(&self) -> SystemTime {
        const NANOS_PER_SEC: u64 = 1_000_000_000;
        let Ok(old_frac) = u64::try_from(self.fraction.into()) else {
            return UNIX_EPOCH;
        };
        let Ok(old_hz) = u64::try_from(self.hertz.into()) else {
            return UNIX_EPOCH;
        };
        let nanos = mul_div_ceil(old_frac, NANOS_PER_SEC, old_hz).unwrap_or(NANOS_PER_SEC);
        // Saturate seconds to avoid Duration overflow.
        let secs = self.seconds.min(i64::MAX as u64);
        UNIX_EPOCH
            .checked_add(Duration::new(secs, 0))
            .and_then(|t| t.checked_add(Duration::from_nanos(nanos)))
            .unwrap_or(UNIX_EPOCH)
    }

    // -----------------------------------------------------------------------
    // Simple serialisation (big-endian u64 triples; canoto parity deferred)
    // -----------------------------------------------------------------------

    /// Encodes `self` as a 24-byte big-endian buffer: `[seconds(8),
    /// fraction(8), hertz(8)]`.
    ///
    /// Full Go-canoto byte-level parity is deferred to M7.8 types (which owns
    /// the persisted execution blob). This encoding is sufficient for
    /// round-trip tests within the Rust codebase.
    #[must_use]
    pub fn encode(&self) -> [u8; 24] {
        let frac_u64 = u64::try_from(self.fraction.into()).unwrap_or(0);
        let hz_u64 = u64::try_from(self.hertz.into()).unwrap_or(0);
        let mut buf = [0u8; 24];
        buf[..8].copy_from_slice(&self.seconds.to_be_bytes());
        buf[8..16].copy_from_slice(&frac_u64.to_be_bytes());
        buf[16..24].copy_from_slice(&hz_u64.to_be_bytes());
        buf
    }

    /// Decodes a `Time<D>` from a 24-byte big-endian buffer produced by
    /// [`encode`].
    ///
    /// # Errors
    ///
    /// Returns `Err(&'static str)` if the buffer is the wrong length, hertz is
    /// zero, or the encoded fraction is >= hertz.
    pub fn decode(buf: &[u8]) -> Result<Self, &'static str> {
        if buf.len() != 24 {
            return Err("expected 24 bytes");
        }
        let Ok(s_bytes) = <[u8; 8]>::try_from(&buf[..8]) else {
            return Err("bad seconds bytes");
        };
        let Ok(f_bytes) = <[u8; 8]>::try_from(&buf[8..16]) else {
            return Err("bad fraction bytes");
        };
        let Ok(h_bytes) = <[u8; 8]>::try_from(&buf[16..24]) else {
            return Err("bad hertz bytes");
        };
        let seconds = u64::from_be_bytes(s_bytes);
        let frac_u64 = u64::from_be_bytes(f_bytes);
        let hz_u64 = u64::from_be_bytes(h_bytes);
        if hz_u64 == 0 {
            return Err("hertz must be > 0");
        }
        if frac_u64 >= hz_u64 {
            return Err("fraction must be < hertz");
        }
        let hertz = D::from(hz_u64);
        let frac = D::from(frac_u64);
        Ok(Self {
            seconds,
            fraction: frac,
            hertz,
        })
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn is_future(&self, sec: u64, num: D) -> bool {
        if sec != self.seconds {
            return sec > self.seconds;
        }
        num > self.fraction
    }
}
