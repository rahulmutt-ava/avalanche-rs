// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden-vector tests for the AP3 base-fee window + AP4 block gas cost
//! (M6.11, spec 21 §4). The vectors in `tests/vectors/cchain/fees/{ap3,ap4}/`
//! are the worked examples documented in spec 21 §4 (the Go reference is coreth
//! `plugin/evm/customheader/{dynamic_fee_windower,block_gas_cost}.go`); see each
//! file's `_provenance` field. This test asserts the Rust fee math matches them
//! bit-for-bit, plus property tests for the saturating window invariants.

use ava_evm::feerules::blockgas::{
    BLOCK_GAS_COST_STEP_AP4, BLOCK_GAS_COST_STEP_AP5, MAX_BLOCK_GAS_COST, ap4_block_gas_cost,
    block_gas_cost,
};
use ava_evm::feerules::window::{BaseFeeParams, Window, base_fee_from_window};
use proptest::prelude::*;
use ruint::aliases::U256;
use serde_json::Value;

fn params_for(phase: &str) -> BaseFeeParams {
    match phase {
        "ap3" => BaseFeeParams::ap3(),
        "ap4" => BaseFeeParams::ap4(),
        "ap5" => BaseFeeParams::ap5(),
        "etna" => BaseFeeParams::etna(),
        other => panic!("unknown phase {other}"),
    }
}

fn single_slot(total: u64) -> Window {
    let mut w = Window::default();
    w.0[9] = total;
    w
}

fn u256_str(v: &Value) -> U256 {
    U256::from_str_radix(v.as_str().expect("string wei"), 10).expect("parse wei")
}

#[test]
fn ap3_base_fee_matches_spec_vectors() {
    let raw = include_str!("vectors/cchain/fees/ap3/base_fee.json");
    let doc: Value = serde_json::from_str(raw).expect("parse ap3 vectors");
    for case in doc["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("name");
        let params = params_for(case["phase"].as_str().expect("phase"));
        let window = single_slot(case["window_sum"].as_u64().expect("window_sum"));
        let parent = u256_str(&case["parent_base_fee_wei"]);
        let dt = case["time_elapsed"].as_u64().expect("time_elapsed");
        let want = u256_str(&case["expected_base_fee_wei"]);

        let got = base_fee_from_window(params, &window, parent, dt);
        assert_eq!(got, want, "ap3 base_fee case {name}");
    }
}

#[test]
fn ap4_block_gas_cost_matches_spec_vectors() {
    let raw = include_str!("vectors/cchain/fees/ap4/block_gas_cost.json");
    let doc: Value = serde_json::from_str(raw).expect("parse ap4 vectors");
    for case in doc["cases"].as_array().expect("cases array") {
        let name = case["name"].as_str().expect("name");
        let parent_cost = case["parent_cost"].as_u64();
        let step = case["step"].as_u64().expect("step");
        let dt = case["time_elapsed"].as_u64().expect("time_elapsed");
        let granite = case["granite"].as_bool().expect("granite");
        let want = case["expected"].as_u64().expect("expected");

        let got = block_gas_cost(parent_cost, step, dt, granite);
        assert_eq!(got, want, "ap4 block_gas_cost case {name}");
    }
}

// Property tests: the saturating window + clamp invariants must hold for all
// inputs (no panics, output always within documented bounds).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn feerules_window_sum_never_panics_and_bounded(slots in any::<[u64; 10]>()) {
        let w = Window(slots);
        let s = w.sum();
        // sum is saturating: <= u64::MAX always, and >= max single slot.
        let max_slot = slots.iter().copied().max().unwrap_or(0);
        prop_assert!(s >= max_slot);
    }

    #[test]
    fn feerules_window_shift_zeroes_front(slots in any::<[u64; 10]>(), n in 0u64..15) {
        let mut w = Window(slots);
        w.shift(n);
        if n >= 10 {
            prop_assert_eq!(w, Window::default());
        } else {
            let n = n as usize;
            // The front (newly vacated) slots are zero.
            for v in &w.0[10 - n..] {
                prop_assert_eq!(*v, 0);
            }
            // The surviving tail moved to the front intact.
            prop_assert_eq!(&w.0[..10 - n], &slots[n..]);
        }
    }

    #[test]
    fn feerules_ap4_block_gas_cost_bounded(parent in any::<u64>(), step in any::<u64>(), dt in 0u64..50) {
        let got = ap4_block_gas_cost(Some(parent), step, dt);
        prop_assert!(got <= MAX_BLOCK_GAS_COST);
        // None always yields 0.
        prop_assert_eq!(ap4_block_gas_cost(None, step, dt), 0);
    }

    #[test]
    fn feerules_base_fee_within_bounds_when_off_target(
        sum in any::<u64>(),
        parent_lo in any::<u64>(),
        dt in 0u64..100,
    ) {
        let p = BaseFeeParams::ap5();
        // Keep the parent base fee inside [min, max] so the only escape from
        // bounds is the exact-target early return (trap 1).
        let parent = p.min_base_fee.saturating_add(U256::from(parent_lo));
        let w = single_slot(sum);
        let got = base_fee_from_window(p, &w, parent, dt);
        if sum == p.target_gas {
            // Trap 1: exact target returns parent unclamped.
            prop_assert_eq!(got, parent);
        } else {
            prop_assert!(got >= p.min_base_fee);
            prop_assert!(got <= p.max_base_fee);
        }
    }
}

#[test]
fn ap4_ap5_step_constants_distinct() {
    // Guard against an accidental constant swap.
    assert_eq!(BLOCK_GAS_COST_STEP_AP4, 50_000);
    assert_eq!(BLOCK_GAS_COST_STEP_AP5, 200_000);
}
