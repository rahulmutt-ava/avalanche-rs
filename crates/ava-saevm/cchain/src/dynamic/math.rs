// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `dynamic::math` — shared helpers for exponential-integrator exponent types:
//! [`toward`] (clamped one-block step) and [`search`] (binary-search
//! inversion). Port of Go `vms/saevm/cchain/dynamic/math.go`.

/// Moves `current` at most `max_diff` steps toward `desired`.
///
/// - If `desired` is `None`, `current` is returned unchanged.
/// - Otherwise the absolute difference is clamped to `max_diff` and applied
///   in the correct direction.
///
/// Port of Go `toward[T ~uint64](current T, desired *T, maxDiff T) T`.
#[must_use]
pub(super) fn toward(current: u64, desired: Option<u64>, max_diff: u64) -> u64 {
    let Some(d) = desired else {
        return current;
    };

    // change = min(|current - d|, max_diff)
    let change = if current >= d {
        current.saturating_sub(d).min(max_diff)
    } else {
        d.saturating_sub(current).min(max_diff)
    };

    if current < d {
        // move up; current + change <= d <= u64::MAX, so no overflow
        current.saturating_add(change)
    } else {
        // move down; current - change >= 0, so no underflow
        current.saturating_sub(change)
    }
}

/// Returns the smallest `v` in `[0, n)` for which `f(v)` is true, assuming
/// `f` is monotonic (if `f(v)` then `f(w)` for all `w >= v`). Returns `n` if
/// no such `v` exists.
///
/// Port of Go `search[T ~uint64](n T, f func(T) bool) T`.
///
/// The midpoint is computed as `lo + (hi - lo) / 2` to avoid overflow when
/// `lo + hi` would exceed `u64::MAX`.
#[must_use]
pub(super) fn search(n: u64, f: impl Fn(u64) -> bool) -> u64 {
    let mut lo: u64 = 0;
    let mut hi: u64 = n;
    while lo < hi {
        // Overflow-safe midpoint (mirrors Go's `lo + (hi-lo)/2`).
        let mid = lo.wrapping_add(hi.wrapping_sub(lo).wrapping_div(2));
        if f(mid) {
            hi = mid;
        } else {
            // mid < hi (since mid < lo+1 only if lo==hi, but loop ensures lo<hi),
            // so mid + 1 <= hi <= u64::MAX.
            lo = mid.saturating_add(1);
        }
    }
    lo
}

// ---------------------------------------------------------------------------
// Unit tests for the helpers (named so `test(dynamic)` filter matches).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{search, toward};

    #[test]
    fn dynamic_toward_nil_unchanged() {
        assert_eq!(toward(42, None, 100), 42);
    }

    #[test]
    fn dynamic_toward_increase_clamped() {
        assert_eq!(toward(0, Some(500), 200), 200);
    }

    #[test]
    fn dynamic_toward_decrease_clamped() {
        assert_eq!(toward(500, Some(0), 200), 300);
    }

    #[test]
    fn dynamic_toward_exact() {
        assert_eq!(toward(100, Some(200), 200), 200);
    }

    #[test]
    fn dynamic_search_first_true() {
        // f(v) = v >= 3; search in [0, 10)
        let result = search(10, |v| v >= 3);
        assert_eq!(result, 3);
    }

    #[test]
    fn dynamic_search_none_true() {
        // f is always false; returns n
        let result = search(10, |_| false);
        assert_eq!(result, 10);
    }

    #[test]
    fn dynamic_search_all_true() {
        // f(0) = true; returns 0
        let result = search(10, |_| true);
        assert_eq!(result, 0);
    }
}
