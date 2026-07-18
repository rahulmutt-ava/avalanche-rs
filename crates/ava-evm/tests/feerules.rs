// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden-vector tests for the AP3 base-fee window + AP4 block gas cost
//! (M6.11, spec 21 §4). The vectors in `tests/vectors/cchain/fees/{ap3,ap4}/`
//! are the worked examples documented in spec 21 §4 (the Go reference is coreth
//! `plugin/evm/customheader/{dynamic_fee_windower,block_gas_cost}.go`); see each
//! file's `_provenance` field. This test asserts the Rust fee math matches them
//! bit-for-bit, plus property tests for the saturating window invariants.

use ava_evm::block::AvaHeader;
use ava_evm::chainspec::{AvaChainSpec, CChainGenesis, NetworkUpgrades};
use ava_evm::feerules;
use ava_evm::feerules::blockgas::{
    BLOCK_GAS_COST_STEP_AP4, BLOCK_GAS_COST_STEP_AP5, MAX_BLOCK_GAS_COST, ap4_block_gas_cost,
    block_gas_cost,
};
use ava_evm::feerules::window::{BaseFeeParams, Window, base_fee_from_window};
use ava_evm_reth::{Bytes, Chain};
use ava_types::constants::LOCAL_ID;
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

// ─── verify_gas_limit: per-fork VerifyGasLimit arms (M9.15 task 2) ────────────

/// A chain spec with every fork (including Fortuna+Granite) active at genesis —
/// the local-network genesis, mirroring `fee_schedule.rs::local_all_active_spec()`.
fn local_all_active_spec() -> AvaChainSpec {
    let genesis = CChainGenesis::parse(include_str!("vectors/cchain/genesis/local.json"))
        .expect("parse local genesis");
    AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(genesis.chain_id()))
}

/// A schedule with each pre-Fortuna fork at a distinct activation timestamp, so
/// a single spec can select any pre-Fortuna arm by varying `header.time` —
/// mirrors `fee_schedule.rs::staged_schedule()`.
fn phase_staggered_spec() -> AvaChainSpec {
    AvaChainSpec::from_parts(
        NetworkUpgrades {
            apricot_phase_1: 1_000,
            apricot_phase_2: 2_000,
            apricot_phase_3: 3_000,
            apricot_phase_4: 4_000,
            apricot_phase_5: 5_000,
            apricot_phase_pre_6: 6_000,
            apricot_phase_6: 7_000,
            apricot_phase_post_6: 8_000,
            banff: 9_000,
            cortina: 10_000,
            durango: 11_000,
            etna: 12_000,
            fortuna: 13_000,
            granite: 14_000,
            helicon: u64::MAX,
        },
        Chain::from_id(43_114),
        false,
    )
}

/// The 24-byte ACP-176 fee-state extra prefix: capacity(8) | excess(8) |
/// target_excess(8), all big-endian.
fn acp176_extra(capacity: u64, excess: u64, target_excess: u64) -> Bytes {
    let mut e = Vec::with_capacity(24);
    e.extend_from_slice(&capacity.to_be_bytes());
    e.extend_from_slice(&excess.to_be_bytes());
    e.extend_from_slice(&target_excess.to_be_bytes());
    e.into()
}

/// coreth `customheader/gas_limit.go:101-160` — `VerifyGasLimit` per-fork arms.
#[test]
fn verify_gas_limit_fortuna_equality() {
    // `local_all_active_spec`'s "genesis" is the local network's real
    // `InitiallyActiveTime` (2020-12-05 05:00:00 UTC = 1_607_144_400, spec 10
    // §7.4), not unix-epoch 0 — see `fee_schedule.rs`'s identical convention.
    const GENESIS: u64 = 1_607_144_400;
    let cs = local_all_active_spec();
    let parent = AvaHeader {
        number: 1,
        time: GENESIS,
        extra: acp176_extra(2_000_000, 0, 1_500_000),
        ..AvaHeader::default()
    };
    let want = feerules::fee_state_before_block(&cs, &parent, (GENESIS + 2) * 1000)
        .expect("pre-block state")
        .max_capacity()
        .0;
    let ok = AvaHeader {
        number: 2,
        time: GENESIS + 2,
        gas_limit: want,
        ..AvaHeader::default()
    };
    feerules::verify_gas_limit(&cs, &parent, &ok).expect("exact MaxCapacity accepted");

    let bad = AvaHeader {
        gas_limit: want + 1,
        ..ok
    };
    let err = feerules::verify_gas_limit(&cs, &parent, &bad).expect_err("off-by-one rejected");
    // gas_limit.go:114-119 — `"%w: have %d, want %d"`.
    assert_eq!(
        err.to_string(),
        format!("invalid gas limit: have {}, want {want}", want + 1),
        "sentinel parity: {err}"
    );
}

/// coreth `customheader/gas_limit.go:101-160` — the Cortina static-limit arm.
#[test]
fn verify_gas_limit_cortina_is_15m() {
    let cs = phase_staggered_spec();
    let parent = AvaHeader {
        number: 1,
        time: 10_000,
        ..AvaHeader::default()
    };
    let ok = AvaHeader {
        number: 2,
        time: 10_500, // Cortina-active, pre-Durango.
        gas_limit: 15_000_000,
        ..AvaHeader::default()
    };
    feerules::verify_gas_limit(&cs, &parent, &ok).expect("exact CortinaGasLimit accepted");

    let bad = AvaHeader {
        gas_limit: 15_000_001,
        ..ok
    };
    let err = feerules::verify_gas_limit(&cs, &parent, &bad).expect_err("off-by-one rejected");
    // gas_limit.go:123-127 — `"%w: expected to be %d in Cortina, but found %d"`.
    assert_eq!(
        err.to_string(),
        "invalid gas limit: expected to be 15000000 in Cortina, but found 15000001",
        "sentinel parity: {err}"
    );
}

/// coreth `customheader/gas_limit.go:101-160` — the ApricotPhase1 static-limit
/// arm.
#[test]
fn verify_gas_limit_ap1_is_8m() {
    let cs = phase_staggered_spec();
    let parent = AvaHeader {
        number: 1,
        time: 1_000,
        ..AvaHeader::default()
    };
    let ok = AvaHeader {
        number: 2,
        time: 1_500, // ApricotPhase1-active, pre-ApricotPhase2.
        gas_limit: 8_000_000,
        ..AvaHeader::default()
    };
    feerules::verify_gas_limit(&cs, &parent, &ok).expect("exact ApricotPhase1GasLimit accepted");

    let bad = AvaHeader {
        gas_limit: 8_000_001,
        ..ok
    };
    let err = feerules::verify_gas_limit(&cs, &parent, &bad).expect_err("off-by-one rejected");
    // gas_limit.go:131-135 — `"%w: expected to be %d in ApricotPhase1, but found %d"`.
    assert_eq!(
        err.to_string(),
        "invalid gas limit: expected to be 8000000 in ApricotPhase1, but found 8000001",
        "sentinel parity: {err}"
    );
}

/// coreth `customheader/gas_limit.go:138-145` / `plugin/evm/upgrade/ap0/params.go:27-28`
/// — the pre-AP1 `[MinGasLimit, MaxGasLimit]` range arm.
#[test]
fn verify_gas_limit_ap0_range() {
    let cs = phase_staggered_spec();

    // Each parent's gas limit matches its header's own, so the (separately
    // tested, gas_limit.go:147-157) bound-divisor arm trivially passes
    // (diff == 0) and only the range check below is exercised.
    let parent_min = AvaHeader {
        number: 1,
        time: 100,
        gas_limit: 5_000,
        ..AvaHeader::default()
    };
    let min_ok = AvaHeader {
        number: 2,
        time: 500, // pre-ApricotPhase1.
        gas_limit: 5_000,
        ..AvaHeader::default()
    };
    feerules::verify_gas_limit(&cs, &parent_min, &min_ok).expect("AP0 min bound accepted");

    let parent_max = AvaHeader {
        gas_limit: 0x7fff_ffff_ffff_ffff,
        ..parent_min.clone()
    };
    let max_ok = AvaHeader {
        gas_limit: 0x7fff_ffff_ffff_ffff,
        ..min_ok.clone()
    };
    feerules::verify_gas_limit(&cs, &parent_max, &max_ok).expect("AP0 max bound accepted");

    let bad = AvaHeader {
        gas_limit: 4_999,
        ..min_ok
    };
    // The range check (go:138-145) runs BEFORE the bound-divisor check, so
    // this is rejected on the range regardless of parent_min's gas limit.
    let err =
        feerules::verify_gas_limit(&cs, &parent_min, &bad).expect_err("below AP0 min rejected");
    // gas_limit.go:139-144 — `"%w: %d not in range [%d, %d]"`.
    assert_eq!(
        err.to_string(),
        "invalid gas limit: 4999 not in range [5000, 9223372036854775807]",
        "sentinel parity: {err}"
    );
}

/// coreth `customheader/gas_limit.go:147-157` — the pre-AP1 bound-divisor arm:
/// the gas limit may not jump by `>= parent.GasLimit / GasLimitBoundDivisor`
/// from the parent's, even when the claimed value is itself in range.
#[test]
fn verify_gas_limit_ap0_bound_divisor() {
    let cs = phase_staggered_spec();
    let parent = AvaHeader {
        number: 1,
        time: 100,
        gas_limit: 8_000_000,
        ..AvaHeader::default()
    };
    let limit = parent.gas_limit / feerules::AP0_GAS_LIMIT_BOUND_DIVISOR; // 7_812

    // diff == limit - 1: strictly under the threshold, accepted.
    let small_delta = AvaHeader {
        number: 2,
        time: 500, // pre-ApricotPhase1.
        gas_limit: parent.gas_limit + limit - 1,
        ..AvaHeader::default()
    };
    feerules::verify_gas_limit(&cs, &parent, &small_delta).expect("small jump accepted");

    // diff == limit: Go's `diff >= limit` triggers on equality too.
    let boundary = AvaHeader {
        gas_limit: parent.gas_limit + limit,
        ..small_delta
    };
    let err = feerules::verify_gas_limit(&cs, &parent, &boundary)
        .expect_err("jump == parent/1024 rejected");
    // gas_limit.go:151-156 — `"%w: have %d, want %d += %d"`.
    assert_eq!(
        err.to_string(),
        format!(
            "invalid gas limit: have {}, want {} += {limit}",
            parent.gas_limit + limit,
            parent.gas_limit
        ),
        "sentinel parity: {err}"
    );
}

// ─── verify_extra_prefix: `VerifyExtraPrefix` per-fork arms (M9.15 task 3) ────

/// coreth `customheader/extra.go:62-111` — `VerifyExtraPrefix`, Fortuna arm:
/// the claimed ACP-176 state must equal `feeStateAfterBlock(parent, header,
/// claimed.TargetExcess)` — passing the CLAIMED target excess means the
/// expectation clamps toward the claim, so any one-step-reachable claim is
/// accepted and anything else mismatches (extra.go:74-87).
#[test]
fn verify_extra_prefix_fortuna_honest_and_tampered() {
    // `local_all_active_spec`'s real local-network genesis is
    // `1_607_144_400` (see `verify_gas_limit_fortuna_equality`'s identical
    // convention) — using an epoch-relative timestamp here would put
    // `header.time` before the network's actual Fortuna activation.
    const GENESIS: u64 = 1_607_144_400;
    let cs = local_all_active_spec();
    let parent = AvaHeader {
        number: 1,
        time: GENESIS,
        extra: acp176_extra(2_000_000, 0, 1_500_000),
        ..AvaHeader::default()
    };

    // Honest child: extra prefix = fee_state_after_block with its own target.
    let honest_state = feerules::fee_state_after_block(
        &cs,
        &parent,
        GENESIS + 2,
        Some((GENESIS + 2) * 1000),
        21_000,
        0,
        None,
    )
    .expect("after-block state");
    let child = AvaHeader {
        number: 2,
        time: GENESIS + 2,
        time_milliseconds: Some((GENESIS + 2) * 1000),
        gas_used: 21_000,
        ext_data_gas_used: Some(U256::ZERO),
        extra: honest_state.to_bytes().to_vec().into(),
        ..AvaHeader::default()
    };
    feerules::verify_extra_prefix(&cs, &parent, &child).expect("honest prefix accepted");

    // Tampered: flip a byte inside the excess field (bytes 8..16 of the prefix).
    let mut tampered_extra = child.extra.to_vec();
    tampered_extra[9] ^= 0x01;
    let tampered = AvaHeader {
        extra: tampered_extra.into(),
        ..child
    };
    let err = feerules::verify_extra_prefix(&cs, &parent, &tampered)
        .expect_err("tampered prefix rejected");
    assert!(
        err.to_string().contains("incorrect fee state"),
        "sentinel parity: {err}"
    );
}

/// A claimed target excess reachable in one step is accepted (the clamp makes
/// expected == claimed); a claim beyond the per-block step mismatches.
#[test]
fn verify_extra_prefix_target_excess_clamp() {
    // See `verify_extra_prefix_fortuna_honest_and_tampered` for why the real
    // local-network genesis timestamp is used rather than an epoch-relative one.
    const GENESIS: u64 = 1_607_144_400;
    let cs = local_all_active_spec();
    let parent = AvaHeader {
        number: 1,
        time: GENESIS,
        extra: acp176_extra(2_000_000, 0, 1_500_000),
        ..AvaHeader::default()
    };

    // Reachable claim: recompute with a slightly-moved desired target.
    let near = feerules::fee_state_after_block(
        &cs,
        &parent,
        GENESIS + 2,
        Some((GENESIS + 2) * 1000),
        0,
        0,
        Some(1_500_001),
    )
    .expect("near-claim state");
    assert_eq!(near.target_excess.0, 1_500_001, "one-step-reachable claim");
    let child_near = AvaHeader {
        number: 2,
        time: GENESIS + 2,
        time_milliseconds: Some((GENESIS + 2) * 1000),
        extra: near.to_bytes().to_vec().into(),
        ..AvaHeader::default()
    };
    feerules::verify_extra_prefix(&cs, &parent, &child_near).expect("reachable claim accepted");

    // Unreachable claim: hand-craft a prefix whose target_excess jumped far
    // beyond one step; the clamped expectation cannot equal it.
    let far = acp176_extra(2_000_000, 0, u64::MAX / 2);
    let child_far = AvaHeader {
        extra: far,
        ..child_near
    };
    let err = feerules::verify_extra_prefix(&cs, &parent, &child_far)
        .expect_err("unreachable claim rejected");
    assert!(err.to_string().contains("incorrect fee state"));
}

/// coreth `customheader/extra.go:62-111` — `VerifyExtraPrefix`, `[AP3, Fortuna)`
/// window arm: `header.Extra` must start with the recomputed fee window's
/// bytes (`extra.go:96-108`), but may carry additional trailing bytes
/// (predicate/padding) after the prefix.
#[test]
fn verify_extra_prefix_window_arm() {
    let cs = phase_staggered_spec();
    // Pre-AP3 parent (AP3 activates at 3_000): `fee_window` seeds the default
    // (all-zero) window regardless of `parent.extra`.
    let parent = AvaHeader {
        number: 1,
        time: 2_000,
        ..AvaHeader::default()
    };
    let window = feerules::fee_window(&cs, &parent, 3_500).expect("fee window");
    let mut extra = window.to_bytes().to_vec();
    extra.extend_from_slice(&[0xAB; 6]); // trailing predicate/padding bytes.
    let child = AvaHeader {
        number: 2,
        time: 3_500, // AP3-active, pre-Fortuna.
        extra: extra.clone().into(),
        ..AvaHeader::default()
    };
    feerules::verify_extra_prefix(&cs, &parent, &child).expect("honest window prefix accepted");

    let mut tampered_extra = extra;
    tampered_extra[0] ^= 0x01;
    let tampered = AvaHeader {
        extra: tampered_extra.into(),
        ..child
    };
    let err = feerules::verify_extra_prefix(&cs, &parent, &tampered)
        .expect_err("tampered window prefix rejected");
    assert!(
        err.to_string().contains("invalid header.Extra prefix"),
        "sentinel parity: {err}"
    );
}
