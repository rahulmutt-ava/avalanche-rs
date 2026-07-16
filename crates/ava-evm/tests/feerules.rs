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

// ─── ACP-176 fee-state extra prefix golden (M9.15 task 4) ─────────────────────

/// The strongest ground truth for `fee_state_after_block`: the recorded live Go
/// C-Chain block 1 carries, in its header `Extra` prefix, the exact 24-byte
/// ACP-176 fee state Go itself computed via `customheader.ExtraPrefix` →
/// `feeStateAfterBlock`. Recompute that state from the (genesis) parent + block-1
/// header fields in Rust and assert the bytes match coreth's byte-for-byte.
///
/// The recorded fixture is the local network (Etna→Granite all active at
/// genesis), so block 1 is Granite-active and its extra opens with the 24-byte
/// state (30 bytes total: 24 state + 6 predicate/padding bytes coreth appends).
#[test]
fn fee_state_after_block_matches_live_go_block_extra() {
    use ava_evm::block::decode_ava_evm_block;
    use ava_evm::chainspec::{AvaChainSpec, CChainGenesis};
    use ava_evm::feerules::acp176::STATE_SIZE;
    use ava_evm::feerules::fee_state_after_block;
    use ava_evm_reth::{B256, Chain};
    use ava_types::constants::LOCAL_ID;

    // The unsigned post-fork proposervm container: cert length at [54..58] (0),
    // inner coreth block length at [58..62], then the block bytes.
    fn inner_block_of(container: &[u8]) -> &[u8] {
        let block_len =
            u32::from_be_bytes(container[58..62].try_into().expect("block len")) as usize;
        &container[62..62 + block_len]
    }

    let vector: Value = serde_json::from_str(include_str!(
        "vectors/cchain/block_wire/live_local_block1.json"
    ))
    .expect("live_local_block1.json parses");
    let container = hex::decode(vector["container_hex"].as_str().expect("container_hex"))
        .expect("container hex decodes");
    let inner = inner_block_of(&container);

    let genesis = CChainGenesis::parse(include_str!("vectors/cchain/genesis/local.json"))
        .expect("parse local genesis");
    let spec = AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(genesis.chain_id()));

    // Block 1's parent is the genesis header. `fee_state_after_block` seeds the
    // zero ACP-176 state for a genesis (number-0) parent, so the genesis state
    // root passed here is irrelevant to the fee state — only its time / number
    // matter. (This is exactly coreth `feeStateBeforeBlock`'s `parent.Number ==
    // 0 => zero state` branch.)
    let genesis_header = genesis.genesis_header(B256::ZERO, spec.network_upgrades());
    assert_eq!(genesis_header.number, 0, "genesis parent is height 0");

    let block1 = decode_ava_evm_block(inner, &spec).expect("decode live inner block 1");
    let h = block1.header();
    assert_eq!(h.number, 1, "fixture inner block is height 1");
    assert!(
        h.extra.len() >= STATE_SIZE,
        "Fortuna+ block extra carries at least the 24-byte fee state (got {})",
        h.extra.len()
    );

    let expected = &h.extra[..STATE_SIZE];
    let ext_data_gas_used = h
        .ext_data_gas_used
        .map(|v| u64::try_from(v).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let got = fee_state_after_block(
        &spec,
        &genesis_header,
        h.time,
        h.time_milliseconds,
        h.gas_used,
        ext_data_gas_used,
        None,
    )
    .expect("fee_state_after_block");

    assert_eq!(
        got.to_bytes().as_slice(),
        expected,
        "coreth feeStateAfterBlock parity: recomputed ACP-176 state must equal the \
         live Go block's extra prefix (Go computed this via customheader.ExtraPrefix)"
    );
}
