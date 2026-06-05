// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Checked arithmetic (`safemath`) — never silent-wrap (determinism hazard #3).
//!
//! Generic checked `add`/`sub`/`mul` over unsigned integers returning
//! [`Error::Overflow`] / [`Error::Underflow`], plus `abs_diff` and
//! `max_uint::<T>()`. Mirrors Go `utils/math/safe_math.go`.
//! Owning spec: `specs/03-core-primitives.md` §4.3, `specs/00` §6.1.

use crate::error::{Error, Result};

/// Unsigned integer types usable with the checked arithmetic helpers.
///
/// Implemented for the standard unsigned primitives; the methods delegate to the
/// inherent `checked_*` / `abs_diff` operations so there is no silent wrap.
pub trait UnsignedInt: Copy + PartialOrd {
    /// The maximum value of the type (Go `MaxUint`).
    const MAX: Self;
    /// `self + rhs`, or `None` on overflow.
    fn checked_add(self, rhs: Self) -> Option<Self>;
    /// `self - rhs`, or `None` on underflow.
    fn checked_sub(self, rhs: Self) -> Option<Self>;
    /// `self * rhs`, or `None` on overflow.
    fn checked_mul(self, rhs: Self) -> Option<Self>;
    /// Absolute difference `|self - rhs|` (never overflows for unsigned).
    fn abs_diff(self, rhs: Self) -> Self;
}

macro_rules! impl_unsigned_int {
    ($($t:ty),+ $(,)?) => {
        $(
            impl UnsignedInt for $t {
                const MAX: Self = <$t>::MAX;
                #[inline]
                fn checked_add(self, rhs: Self) -> Option<Self> { <$t>::checked_add(self, rhs) }
                #[inline]
                fn checked_sub(self, rhs: Self) -> Option<Self> { <$t>::checked_sub(self, rhs) }
                #[inline]
                fn checked_mul(self, rhs: Self) -> Option<Self> { <$t>::checked_mul(self, rhs) }
                #[inline]
                fn abs_diff(self, rhs: Self) -> Self { <$t>::abs_diff(self, rhs) }
            }
        )+
    };
}

impl_unsigned_int!(u8, u16, u32, u64, u128, usize);

/// Checked addition (Go `math.Add`). Returns [`Error::Overflow`] on overflow.
///
/// # Errors
/// Returns [`Error::Overflow`] if `a + b` does not fit in `T`.
pub fn add<T: UnsignedInt>(a: T, b: T) -> Result<T> {
    a.checked_add(b).ok_or(Error::Overflow)
}

/// Checked subtraction (Go `math.Sub`). Returns [`Error::Underflow`] on underflow.
///
/// # Errors
/// Returns [`Error::Underflow`] if `b > a`.
pub fn sub<T: UnsignedInt>(a: T, b: T) -> Result<T> {
    a.checked_sub(b).ok_or(Error::Underflow)
}

/// Checked multiplication (Go `math.Mul`). Returns [`Error::Overflow`] on overflow.
///
/// # Errors
/// Returns [`Error::Overflow`] if `a * b` does not fit in `T`.
pub fn mul<T: UnsignedInt>(a: T, b: T) -> Result<T> {
    a.checked_mul(b).ok_or(Error::Overflow)
}

/// Absolute difference `|a - b|` (Go `math.AbsDiff`); never overflows.
#[must_use]
pub fn abs_diff<T: UnsignedInt>(a: T, b: T) -> T {
    a.abs_diff(b)
}

/// The maximum value of an unsigned integer type (Go `math.MaxUint`).
#[must_use]
pub fn max_uint<T: UnsignedInt>() -> T {
    T::MAX
}
