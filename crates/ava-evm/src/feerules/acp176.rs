// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fortuna/ACP-176 dynamic fee state machine (spec 21 §5, spec 10 §7.1/§17.3).
//! Mirrors `vms/evm/acp176/acp176.go` bit-for-bit.
//!
//! On-chain state [`Acp176State`] is 24 bytes: `[capacity(8) | excess(8) |
//! target_excess(8)]` big-endian. [`Acp176State::gas_price`] is the only fee
//! the block-builder and validator need; the other accessors drive the state
//! machine forward.
//!
//! The two-level exponential (spec 21 §5):
//! ```text
//! T = CalculatePrice(P, target_excess, D)   // target gas/s
//! K = T * 87                                // price update conversion constant
//! gasPrice = CalculatePrice(M, gas.excess, K)
//! ```
//!
//! All arithmetic is checked/saturating (no floats, spec 00 §6.1).
//! `scaleExcess` rounds **DOWN** (U256 floor) — unlike SAE which rounds up.
//! Do **not** share this routine with any SAE ceil version.

use ruint::aliases::U256;

use super::{Gas, GasState, Price, calculate_price};
use crate::error::Error;

// ─── ACP-176 constants (`vms/evm/acp176/acp176.go`) ──────────────────────────

/// `MinTargetPerSecond` (P) — minimum gas target per second.
pub const MIN_TARGET_PER_SECOND: u64 = 1_000_000;
/// `MaxTargetExcessDiff` (Q) — maximum target excess change per block.
pub const MAX_TARGET_EXCESS_DIFF: u64 = 1 << 15; // 32_768
/// `MaxTargetChangeRate` — controls the rate the target can change per block.
pub const MAX_TARGET_CHANGE_RATE: u64 = 1024;
/// `TargetConversion` (D) = `MaxTargetChangeRate * MaxTargetExcessDiff`.
pub const TARGET_CONVERSION: u64 = MAX_TARGET_CHANGE_RATE * MAX_TARGET_EXCESS_DIFF; // 33_554_432
/// `MinGasPrice` (M) — minimum gas price floor.
pub const MIN_GAS_PRICE: u64 = 1;
/// `TimeToFillCapacity` — seconds to fill from empty to max capacity.
pub const TIME_TO_FILL_CAPACITY: u64 = 5;
/// `TargetToMax` — multiplier from target-per-second to max-per-second.
pub const TARGET_TO_MAX: u64 = 2;
/// `TargetToPriceUpdateConversion` — `K = T * 87` (≈60/ln2 → doubles ~60s).
pub const TARGET_TO_PRICE_UPDATE_CONVERSION: u64 = 87;
/// `TargetToMaxCapacity` = `TargetToMax * TimeToFillCapacity` = 10.
pub const TARGET_TO_MAX_CAPACITY: u64 = TARGET_TO_MAX * TIME_TO_FILL_CAPACITY;

/// `maxTargetExcess` = `TargetConversion * ln(MaxUint64 / MinTargetPerSecond) + 1`.
/// Hard-coded constant from Go (binary-search upper bound for `DesiredTargetExcess`).
pub const MAX_TARGET_EXCESS: u64 = 1_024_950_627;

/// `StateSize` — 3 × 8 bytes (capacity, excess, targetExcess).
pub const STATE_SIZE: usize = 24;

// ─── Error ───────────────────────────────────────────────────────────────────

/// `ErrStateInsufficientLength` — byte slice too short to parse an [`Acp176State`].
#[derive(Debug, thiserror::Error)]
#[error("insufficient length for fee state: expected at least {STATE_SIZE} bytes, got {got}")]
pub struct ErrStateInsufficientLength {
    /// Actual number of bytes provided.
    pub got: usize,
}

// ─── State ────────────────────────────────────────────────────────────────────

/// `acp176.State` — the ACP-176 fee state carried in the block header extra.
///
/// Serialized as 24 big-endian bytes:
/// `[capacity(u64) | excess(u64) | target_excess(u64)]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Acp176State {
    /// Gas capacity + excess state.
    pub gas: GasState,
    /// `TargetExcess` (q) — drives `Target()` via the second exponential.
    pub target_excess: Gas,
}

impl Acp176State {
    // ─── Accessors ────────────────────────────────────────────────────────────

    /// `Target()` — target gas consumed per second.
    ///
    /// `T = CalculatePrice(P, target_excess, D)`
    #[must_use]
    pub fn target(&self) -> Gas {
        Gas(calculate_price(
            Price(MIN_TARGET_PER_SECOND),
            self.target_excess,
            Gas(TARGET_CONVERSION),
        )
        .0)
    }

    /// `MaxCapacity()` — maximum possible accrued gas capacity `C`.
    ///
    /// `C = mulWithUpperBound(T, TargetToMaxCapacity)`
    #[must_use]
    pub fn max_capacity(&self) -> Gas {
        mul_ub(self.target(), Gas(TARGET_TO_MAX_CAPACITY))
    }

    /// `GasPrice()` — current required gas price.
    ///
    /// `K = mulWithUpperBound(T, 87)`, `price = CalculatePrice(M, gas.excess, K)`
    #[must_use]
    pub fn gas_price(&self) -> Price {
        let t = self.target();
        let k = mul_ub(t, Gas(TARGET_TO_PRICE_UPDATE_CONVERSION));
        calculate_price(Price(MIN_GAS_PRICE), self.gas.excess, k)
    }

    // ─── Mutators ─────────────────────────────────────────────────────────────

    /// `AdvanceSeconds(s)` — advance state by `s` whole seconds (Fortuna).
    ///
    /// `R = mulUB(T, 2)`, `C = mulUB(R, 5)`, then `gas.advance(C, R, T, s)`.
    pub fn advance_seconds(&mut self, seconds: u64) {
        let t = self.target();
        let r = mul_ub(t, Gas(TARGET_TO_MAX));
        let c = mul_ub(r, Gas(TIME_TO_FILL_CAPACITY));
        self.gas = self.gas.advance(c, r, t, seconds);
    }

    /// `AdvanceMilliseconds(ms)` — advance state by `ms` milliseconds (Granite).
    ///
    /// Per-ms rates: `targetPerMS = T/1000`, `maxPerMS = targetPerMS*2`; but
    /// `maxCapacity` is still derived from the per-second `R = mulUB(T, 2)`.
    pub fn advance_milliseconds(&mut self, milliseconds: u64) {
        let t = self.target();
        // Integer division: `targetPerMS = T / 1000` (rounds down, matching Go).
        let target_per_ms = Gas(t.0 / 1000);
        // `maxPerMS = targetPerMS * TargetToMax` — can't overflow since 1000 > 2.
        let max_per_ms = Gas(target_per_ms.0.saturating_mul(TARGET_TO_MAX));
        // maxCapacity is derived from the per-second R (same as advance_seconds).
        let max_per_second = mul_ub(t, Gas(TARGET_TO_MAX));
        let max_capacity = mul_ub(max_per_second, Gas(TIME_TO_FILL_CAPACITY));
        self.gas = self
            .gas
            .advance(max_capacity, max_per_ms, target_per_ms, milliseconds);
    }

    /// `ConsumeGas(gasUsed, extraGasUsed)` — subtract from capacity, add to excess.
    ///
    /// `extraGasUsed` must fit in `u64`; a too-large value is treated as
    /// `ErrInsufficientCapacity` (matching Go's `extraGasUsed.IsUint64()` check).
    ///
    /// # Errors
    /// Returns [`Error::FeeOverflow`] if capacity is insufficient (maps
    /// `gas.ErrInsufficientCapacity`, spec 10 §11.2) or if
    /// `extra_gas_used > u64::MAX` (matches Go's `!IsUint64` guard).
    pub fn consume_gas(
        &mut self,
        gas_used: u64,
        extra_gas_used: Option<u128>,
    ) -> core::result::Result<(), Error> {
        let new_gas = self
            .gas
            .consume(Gas(gas_used))
            .map_err(|_| Error::FeeOverflow)?;

        let result = match extra_gas_used {
            None => new_gas,
            Some(extra) => {
                if extra > u64::MAX as u128 {
                    return Err(Error::FeeOverflow);
                }
                new_gas
                    .consume(Gas(extra as u64))
                    .map_err(|_| Error::FeeOverflow)?
            }
        };

        self.gas = result;
        Ok(())
    }

    /// `UpdateTargetExcess(desiredTargetExcess)` — move `target_excess` toward
    /// `desired` by at most Q = 32 768 per block, then rescale gas excess so the
    /// price is continuous, and cap capacity to the new `C`.
    pub fn update_target_excess(&mut self, desired_target_excess: Gas) {
        let old_t = self.target();
        self.target_excess = target_excess_step(self.target_excess, desired_target_excess);
        let new_t = self.target();
        // scaleExcess: floor(excess * new_t / old_t), U256, saturate u64.
        // Rounds DOWN (floor) — unlike SAE which rounds up. Do NOT share.
        self.gas.excess = scale_excess(self.gas.excess, new_t, old_t);
        // Cap capacity to new max capacity.
        let new_max_capacity = mul_ub(new_t, Gas(TARGET_TO_MAX_CAPACITY));
        self.gas.capacity = self.gas.capacity.min(new_max_capacity);
    }

    // ─── Serialization ────────────────────────────────────────────────────────

    /// `Bytes()` — 24-byte big-endian serialization `[capacity|excess|targetExcess]`.
    #[must_use]
    pub fn to_bytes(self) -> [u8; STATE_SIZE] {
        let mut out = [0u8; STATE_SIZE];
        out[0..8].copy_from_slice(&self.gas.capacity.0.to_be_bytes());
        out[8..16].copy_from_slice(&self.gas.excess.0.to_be_bytes());
        out[16..24].copy_from_slice(&self.target_excess.0.to_be_bytes());
        out
    }

    /// `ParseState(bytes)` — deserialize from at least 24 bytes (extras allowed).
    ///
    /// # Errors
    /// Returns [`ErrStateInsufficientLength`] if `bytes.len() < STATE_SIZE`.
    pub fn from_bytes(bytes: &[u8]) -> core::result::Result<Self, ErrStateInsufficientLength> {
        if bytes.len() < STATE_SIZE {
            return Err(ErrStateInsufficientLength { got: bytes.len() });
        }
        let capacity = u64::from_be_bytes(bytes[0..8].try_into().unwrap_or([0u8; 8]));
        let excess = u64::from_be_bytes(bytes[8..16].try_into().unwrap_or([0u8; 8]));
        let target_excess = u64::from_be_bytes(bytes[16..24].try_into().unwrap_or([0u8; 8]));
        Ok(Acp176State {
            gas: GasState {
                capacity: Gas(capacity),
                excess: Gas(excess),
            },
            target_excess: Gas(target_excess),
        })
    }
}

// ─── Free helpers (mirrors acp176.go unexported functions) ────────────────────

/// `mulWithUpperBound(a, b)` — saturating `a * b` (caps at `u64::MAX`).
#[must_use]
pub fn mul_ub(a: Gas, b: Gas) -> Gas {
    Gas(a.0.saturating_mul(b.0))
}

/// `targetExcess(excess, desired)` — move `excess` toward `desired` by at
/// most `MaxTargetExcessDiff` per step.
fn target_excess_step(excess: Gas, desired: Gas) -> Gas {
    let change = excess.0.abs_diff(desired.0).min(MAX_TARGET_EXCESS_DIFF);
    if excess.0 < desired.0 {
        Gas(excess.0 + change)
    } else {
        Gas(excess.0 - change)
    }
}

/// `scaleExcess(excess, newTarget, oldTarget)` — floor(`excess * newTarget / oldTarget`).
///
/// Uses U256 to avoid overflow. Saturates to `u64::MAX` if the result exceeds `u64::MAX`.
///
/// Rounds **down** (floor) — unlike SAE's ceil. Do **not** share with SAE.
#[must_use]
pub fn scale_excess(excess: Gas, new_target: Gas, old_target: Gas) -> Gas {
    if old_target.0 == 0 {
        return excess;
    }
    let big_excess = U256::from(excess.0);
    let big_new = U256::from(new_target.0);
    let big_old = U256::from(old_target.0);
    let result = (big_excess * big_new) / big_old;
    Gas(u64::try_from(result).unwrap_or(u64::MAX))
}

/// `DesiredTargetExcess(desiredTarget)` — binary search over `[0, maxTargetExcess)`
/// for the least `q` with `Target(q) >= desiredTarget`.
#[must_use]
pub fn desired_target_excess(desired_target: Gas) -> Gas {
    // Binary search: find smallest q in [0, MAX_TARGET_EXCESS) with target(q) >= desired.
    let mut lo: u64 = 0;
    let mut hi: u64 = MAX_TARGET_EXCESS;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let probe = Acp176State {
            gas: GasState::default(),
            target_excess: Gas(mid),
        };
        if probe.target() >= desired_target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Gas(lo)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const N_AVAX: u64 = 1_000_000_000;

    // ── Spec 21 §5 worked example 1: baseline target ──────────────────────────

    #[test]
    fn target_at_zero_excess() {
        // targetExcess = 0 => Target = CalculatePrice(1e6, 0, D) = 1e6
        let s = Acp176State::default();
        assert_eq!(s.target(), Gas(1_000_000));
    }

    #[test]
    fn derived_constants_at_baseline() {
        // T = 1e6 => R = 2e6, C = 10e6, K = 87e6
        let s = Acp176State::default();
        let t = s.target();
        assert_eq!(t, Gas(1_000_000));
        assert_eq!(mul_ub(t, Gas(TARGET_TO_MAX)), Gas(2_000_000));
        assert_eq!(
            mul_ub(mul_ub(t, Gas(TARGET_TO_MAX)), Gas(TIME_TO_FILL_CAPACITY)),
            Gas(10_000_000)
        );
        assert_eq!(
            mul_ub(t, Gas(TARGET_TO_PRICE_UPDATE_CONVERSION)),
            Gas(87_000_000)
        );
    }

    // ── Spec 21 §5 worked example 2: price at empty/full excess ───────────────

    #[test]
    fn gas_price_at_zero_excess() {
        // gas.excess = 0 => gasPrice = CalculatePrice(1, 0, K) = 1 = M
        let s = Acp176State::default();
        assert_eq!(s.gas_price(), Price(MIN_GAS_PRICE));
    }

    #[test]
    fn gas_price_at_k_excess_doubles() {
        // gas.excess = K = 87e6 => gasPrice = CalculatePrice(1, K, K) = 2
        // (the x=k doubling identity)
        let k = 87_000_000u64;
        let s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(k),
            },
            target_excess: Gas(0),
        };
        assert_eq!(s.gas_price(), Price(2));
    }

    // ── Spec 21 §5 worked example 3: target excess step clamp ─────────────────

    #[test]
    fn update_target_excess_clamps_step() {
        // From targetExcess=0, desired=1_000_000 => change = min(1M, 32768) = 32768
        // => new targetExcess = 32768
        let mut s = Acp176State::default();
        s.update_target_excess(Gas(1_000_000));
        assert_eq!(s.target_excess, Gas(32_768));
    }

    #[test]
    fn update_target_excess_no_change() {
        let mut s = Acp176State::default();
        s.update_target_excess(Gas(0));
        assert_eq!(s.target_excess, Gas(0));
    }

    #[test]
    fn update_target_excess_max_increase() {
        // From 0, desired > Q => targetExcess = Q = 32768, excess rescaled.
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        s.update_target_excess(Gas(MAX_TARGET_EXCESS_DIFF + 1));
        assert_eq!(s.target_excess, Gas(MAX_TARGET_EXCESS_DIFF));
        // excess rescaled: floor(2_000_000 * newT / oldT)
        // Go test expects 2_001_954
        assert_eq!(s.gas.excess, Gas(2_001_954));
    }

    #[test]
    fn update_target_excess_inverse_max_increase() {
        // Reverse of max_increase should return to ~2_000_000
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_001_954),
            },
            target_excess: Gas(MAX_TARGET_EXCESS_DIFF),
        };
        s.update_target_excess(Gas(0));
        assert_eq!(s.target_excess, Gas(0));
        assert_eq!(s.gas.excess, Gas(2_000_000));
    }

    #[test]
    fn update_target_excess_max_decrease() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000_000),
            },
            target_excess: Gas(2 * MAX_TARGET_EXCESS_DIFF),
        };
        s.update_target_excess(Gas(0));
        assert_eq!(s.target_excess, Gas(MAX_TARGET_EXCESS_DIFF));
        assert_eq!(s.gas.excess, Gas(1_998_047_816));
    }

    #[test]
    fn update_target_excess_reduces_capacity() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(10_019_550),
                excess: Gas(2_000_000_000),
            },
            target_excess: Gas(2 * MAX_TARGET_EXCESS_DIFF),
        };
        s.update_target_excess(Gas(0));
        assert_eq!(s.target_excess, Gas(MAX_TARGET_EXCESS_DIFF));
        assert_eq!(s.gas.capacity, Gas(10_009_770));
        assert_eq!(s.gas.excess, Gas(1_998_047_816));
    }

    #[test]
    fn update_target_excess_overflow_excess() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(u64::MAX),
                excess: Gas(u64::MAX),
            },
            target_excess: Gas(MAX_TARGET_EXCESS - MAX_TARGET_EXCESS_DIFF),
        };
        s.update_target_excess(Gas(MAX_TARGET_EXCESS));
        assert_eq!(s.target_excess, Gas(MAX_TARGET_EXCESS));
        assert_eq!(s.gas.excess, Gas(u64::MAX));
    }

    // ── advance_seconds ────────────────────────────────────────────────────────

    #[test]
    fn advance_seconds_zero() {
        let initial = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        let mut s = initial;
        s.advance_seconds(0);
        assert_eq!(s.gas.capacity, Gas(0));
        assert_eq!(s.gas.excess, Gas(2_000_000));
        assert_eq!(s.target_excess, Gas(13_605_152));
    }

    #[test]
    fn advance_seconds_one() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_seconds(1);
        assert_eq!(s.gas.capacity, Gas(3_000_000));
        assert_eq!(s.gas.excess, Gas(500_000));
        assert_eq!(s.target_excess, Gas(13_605_152));
    }

    #[test]
    fn advance_seconds_five() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(15_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_seconds(5);
        assert_eq!(s.gas.capacity, Gas(15_000_000));
        assert_eq!(s.gas.excess, Gas(7_500_000));
    }

    #[test]
    fn advance_seconds_caps_capacity() {
        // Capacity over max should be capped after advance(0)
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(16_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_seconds(0);
        assert_eq!(s.gas.capacity, Gas(15_000_000));
        assert_eq!(s.gas.excess, Gas(2_000_000));
    }

    // ── advance_milliseconds ───────────────────────────────────────────────────

    #[test]
    fn advance_milliseconds_zero() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_milliseconds(0);
        assert_eq!(s.gas.capacity, Gas(0));
        assert_eq!(s.gas.excess, Gas(2_000_000));
    }

    #[test]
    fn advance_milliseconds_1000() {
        // 1000ms should match advance_seconds(1)
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_milliseconds(1000);
        assert_eq!(s.gas.capacity, Gas(3_000_000));
        assert_eq!(s.gas.excess, Gas(500_000));
    }

    #[test]
    fn advance_milliseconds_5000() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(15_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_milliseconds(5000);
        assert_eq!(s.gas.capacity, Gas(15_000_000));
        assert_eq!(s.gas.excess, Gas(7_500_000));
    }

    #[test]
    fn advance_milliseconds_one() {
        // 1ms with target 1.5M/s: targetPerMS = 1500, maxPerMS = 3000
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_milliseconds(1);
        assert_eq!(s.gas.capacity, Gas(3_000));
        assert_eq!(s.gas.excess, Gas(1_998_500));
    }

    #[test]
    fn advance_milliseconds_caps_capacity() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(16_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_605_152),
        };
        s.advance_milliseconds(0);
        assert_eq!(s.gas.capacity, Gas(15_000_000));
    }

    // ── consume_gas ────────────────────────────────────────────────────────────

    #[test]
    fn consume_gas_no_gas() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        s.consume_gas(0, None).unwrap();
        assert_eq!(s.gas.capacity, Gas(1_000_000));
        assert_eq!(s.gas.excess, Gas(2_000_000));
    }

    #[test]
    fn consume_gas_some() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        s.consume_gas(100_000, None).unwrap();
        assert_eq!(s.gas.capacity, Gas(900_000));
        assert_eq!(s.gas.excess, Gas(2_100_000));
    }

    #[test]
    fn consume_gas_extra() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        s.consume_gas(0, Some(100_000)).unwrap();
        assert_eq!(s.gas.capacity, Gas(900_000));
        assert_eq!(s.gas.excess, Gas(2_100_000));
    }

    #[test]
    fn consume_gas_both() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        s.consume_gas(10_000, Some(100_000)).unwrap();
        assert_eq!(s.gas.capacity, Gas(890_000));
        assert_eq!(s.gas.excess, Gas(2_110_000));
    }

    #[test]
    fn consume_gas_insufficient_capacity() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        let result = s.consume_gas(1_000_001, None);
        assert!(result.is_err());
        // State must be unchanged on error
        assert_eq!(s.gas.capacity, Gas(1_000_000));
        assert_eq!(s.gas.excess, Gas(2_000_000));
    }

    #[test]
    fn consume_gas_massive_extra_overflows_u64() {
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(1_000_000),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(0),
        };
        // extra > u64::MAX => ErrInsufficientCapacity (Go's !IsUint64 guard)
        let result = s.consume_gas(0, Some(u128::MAX));
        assert!(result.is_err());
    }

    // ── serialization (24-byte big-endian) ────────────────────────────────────

    #[test]
    fn bytes_zero_state() {
        let s = Acp176State::default();
        assert_eq!(s.to_bytes(), [0u8; 24]);
    }

    #[test]
    fn bytes_roundtrip() {
        let s = Acp176State {
            gas: GasState {
                capacity: Gas(0x0102030405060708),
                excess: Gas(0x1112131415161718),
            },
            target_excess: Gas(0x2122232425262728),
        };
        let bytes = s.to_bytes();
        let expected = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // capacity
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, // excess
            0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, // target_excess
        ];
        assert_eq!(bytes, expected);
        let back = Acp176State::from_bytes(&bytes).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn parse_insufficient_length() {
        let result = Acp176State::from_bytes(&[0u8; STATE_SIZE - 1]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_truncate_extra_bytes() {
        // Extra bytes past 24 are ignored
        let mut bytes = [0u8; 25];
        bytes[24] = 1;
        let s = Acp176State::from_bytes(&bytes).unwrap();
        assert_eq!(s, Acp176State::default());
    }

    // ── golden vectors from Go readerTests ────────────────────────────────────

    #[test]
    fn golden_reader_tests() {
        // Derived from vms/evm/acp176/acp176_test.go readerTests
        // Format: (gas_excess, target_excess, expected_target, expected_max_capacity, expected_gas_price)
        let cases: &[(u64, u64, u64, u64, u64)] = &[
            // zero
            (0, 0, 1_000_000, 10_000_000, 1),
            // almost_excess_change: 60_303_808, targetExcess=33
            (60_303_808, 33, 1_000_000, 10_000_000, 2),
            // small_excess_change: 60_303_868, targetExcess=34
            (60_303_868, 34, 1_000_001, 10_000_010, 2),
            // current_target: targetExcess=13_605_152, target=1_500_000
            (2_704_386_192, 13_605_152, 1_500_000, 15_000_000, N_AVAX + 2),
            // 3m_target
            (
                6_610_721_802,
                36_863_312,
                3_000_000,
                30_000_000,
                100 * N_AVAX + 4,
            ),
            // 6m_target
            (
                13_221_443_604,
                60_121_472,
                6_000_000,
                60_000_000,
                100 * N_AVAX + 4,
            ),
            // 10m_target
            (
                22_035_739_340,
                77_261_935,
                10_000_000,
                100_000_000,
                100 * N_AVAX + 5,
            ),
            // 100m_target
            (
                220_357_393_400,
                154_523_870,
                100_000_000,
                1_000_000_000,
                100 * N_AVAX + 5,
            ),
        ];

        for &(gas_excess, target_excess, exp_target, exp_max_cap, exp_price) in cases {
            let s = Acp176State {
                gas: GasState {
                    capacity: Gas(0),
                    excess: Gas(gas_excess),
                },
                target_excess: Gas(target_excess),
            };
            assert_eq!(
                s.target(),
                Gas(exp_target),
                "target mismatch for gas_excess={gas_excess} target_excess={target_excess}"
            );
            assert_eq!(
                s.max_capacity(),
                Gas(exp_max_cap),
                "max_capacity mismatch for gas_excess={gas_excess} target_excess={target_excess}"
            );
            assert_eq!(
                s.gas_price(),
                Price(exp_price),
                "gas_price mismatch for gas_excess={gas_excess} target_excess={target_excess}"
            );
        }
    }

    // ── DesiredTargetExcess binary search ─────────────────────────────────────

    #[test]
    fn desired_target_excess_round_trip() {
        // From Go readerTests (non-skip entries): desired = test.target =>
        // DesiredTargetExcess(target) == test.state.TargetExcess
        let cases: &[(u64, u64)] = &[
            (0, 1_000_000),
            (34, 1_000_001),
            (13_605_152, 1_500_000),
            (36_863_312, 3_000_000),
            (60_121_472, 6_000_000),
            (77_261_935, 10_000_000),
            (154_523_870, 100_000_000),
        ];
        for &(expected_excess, desired_target) in cases {
            let got = desired_target_excess(Gas(desired_target));
            assert_eq!(
                got,
                Gas(expected_excess),
                "desired_target_excess({desired_target})"
            );
        }
    }

    // ── scale_excess round-direction: must be floor (DOWN), not ceiling ────────

    #[test]
    fn scale_excess_floor_not_ceil() {
        // excess=3, newT=2, oldT=3: floor(3*2/3) = floor(2.0) = 2
        assert_eq!(scale_excess(Gas(3), Gas(2), Gas(3)), Gas(2));
        // excess=3, newT=1, oldT=2: floor(3*1/2) = floor(1.5) = 1 (floor, not 2)
        assert_eq!(scale_excess(Gas(3), Gas(1), Gas(2)), Gas(1));
    }

    // ── advance_milliseconds target_rounds_down ────────────────────────────────

    #[test]
    fn advance_milliseconds_target_rounds_down() {
        // targetExcess=13_627_491 => target = 1_500_999/s
        // targetPerMS = 1_500_999 / 1000 = 1_500 (integer division, not 1501)
        // after 999ms: capacity = 3_000 * 999 = 2_997_000, excess = 2_000_000 - 1_500 * 999 = 501_500
        let mut s = Acp176State {
            gas: GasState {
                capacity: Gas(0),
                excess: Gas(2_000_000),
            },
            target_excess: Gas(13_627_491),
        };
        s.advance_milliseconds(999);
        assert_eq!(s.gas.capacity, Gas(2_997_000));
        assert_eq!(s.gas.excess, Gas(501_500));
    }
}
