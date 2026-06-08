// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-fork dynamic fee rules: AP3 base-fee window, AP4 block gas cost,
//! Fortuna/ACP-176, ACP-226 (G2, spec 10 §7, spec 21). Populated by
//! M6.11/M6.12/M6.13.
//!
//! The shared EIP-4844 exponential (`CalculatePrice`, spec 21 §0) is owned by
//! `ava-vm`'s ACP-103 gas primitive; it is the same algorithm AP3/Fortuna
//! route through, so it is re-exported here rather than re-derived.

pub mod blockgas;
pub mod window;

// Spec 21 §0: re-export the shared exponential + gas state from the canonical
// owner (`ava_vm::components::gas`) so EVM fee code names one implementation.
pub use ava_vm::components::gas::{Gas, GasState, Price, calculate_price};

#[cfg(test)]
mod calculate_price_tests {
    use super::{Gas, Price, calculate_price};

    /// Spec 21 §0 golden 9-row `CalculatePrice(minPrice, excess, k)` table,
    /// verbatim from `vms/components/gas/gas_test.go`. The last row
    /// (`MaxUint64 − 11`) pins the truncation order bit-exactly.
    #[test]
    fn calculate_price_golden_table() {
        let cases: &[(u64, u64, u64, u64)] = &[
            (1, 0, 1, 1),
            (1, 1, 1, 2),
            (1, 2, 1, 6),
            (1, 10_000, 10_000, 2),
            (1, 1_000_000, 10_000, u64::MAX),
            (10, 10_000_000, 1_000_000, 220_264),
            (u64::MAX, u64::MAX, 1, u64::MAX),
            (4_294_967_295, 1, 1, 11_674_931_546),
            (
                6_786_177_901_268_885_274,
                1,
                1,
                18_446_744_073_709_551_604, // MaxUint64 - 11
            ),
        ];
        for &(m, x, k, want) in cases {
            let got = calculate_price(Price(m), Gas(x), Gas(k));
            assert_eq!(got, Price(want), "calculate_price({m}, {x}, {k})");
        }
    }
}
