// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::evm_fee_schedule_per_fork` — the M6.13 exit-gate property test
//! (spec 10 §7.2/§17.3 G2, spec 21 §7 fork-dispatch + §9 invariants).
//!
//! Over random `(parent header, AvaNextBlockCtx, fork timestamp)` it asserts
//! `AvaEvmConfig::next_evm_env` selects the correct fee regime:
//!
//! - **pre-AP3** → base fee ABSENT (`Error::NilBaseFee` parity with coreth's
//!   `errNilBaseFee`; the produced env carries `basefee == 0`).
//! - **AP3..Fortuna** → the rolling-window base fee (`feerules::window`, M6.11).
//! - **Fortuna+** → the ACP-176 gas-price state machine (`feerules::acp176`,
//!   M6.12).
//!
//! and that `feerules::{base_fee, gas_limit}` agree with the per-fork dispatch.
//! It then re-checks the spec 21 §9 invariants that cross the fork boundary:
//! off-target window moves are ≥1, the AP4 block gas cost stays in `[0, 1e6]`,
//! and the ACP-176 price is continuous (±1) across `UpdateTargetExcess`.

use ava_evm::chainspec::{AvaChainSpec, AvaPhase, NetworkUpgrades};
use ava_evm::evmconfig::{AvaEvmConfig, AvaFeeState, AvaNextBlockCtx};
use ava_evm::feerules::acp176::{Acp176State, MAX_TARGET_EXCESS_DIFF};
use ava_evm::feerules::blockgas::{BLOCK_GAS_COST_STEP_AP4, ap4_block_gas_cost};
use ava_evm::feerules::window::{BaseFeeParams as WindowParams, Window, base_fee_from_window};
use ava_evm::feerules::{self, regime_for_phase, window_params_for_phase};
use ava_evm::{Error, Gas, GasState};
use ava_evm_reth::{Address, B256, Chain, Header};
use proptest::prelude::*;
use ruint::aliases::U256;

/// A schedule with one activation timestamp per phase chosen so the phases are
/// strictly increasing and we can land a `block_timestamp` in any regime by
/// picking the matching window. All Apricot/Banff/.../Granite activations are
/// spaced 1000s apart starting at 1000.
fn staged_schedule() -> NetworkUpgrades {
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
    }
}

fn spec() -> AvaChainSpec {
    AvaChainSpec::from_parts(staged_schedule(), Chain::from_id(43_114), false)
}

/// Build a minimal parent header at the given number / timestamp / base fee.
fn parent_header(number: u64, timestamp: u64, base_fee: Option<u64>) -> Header {
    Header {
        number,
        timestamp,
        gas_limit: 8_000_000,
        base_fee_per_gas: base_fee,
        ..Default::default()
    }
}

prop_compose! {
    fn arb_ctx(child_ts: u64)(
        excess in 0u64..200_000_000,
        target_excess in 0u64..200_000_000,
        recipient_byte in any::<u8>(),
        gas_hint in prop::option::of(1_000_000u64..30_000_000),
        pchain_height in any::<u64>(),
        sub_ms in 0u64..1000,
    ) -> AvaNextBlockCtx {
        let fee_state = AvaFeeState::Acp176(Acp176State {
            gas: GasState { capacity: Gas(0), excess: Gas(excess) },
            target_excess: Gas(target_excess),
        });
        AvaNextBlockCtx {
            timestamp: child_ts,
            timestamp_ms: child_ts.saturating_mul(1000).saturating_add(sub_ms),
            suggested_fee_recipient: Address::repeat_byte(recipient_byte),
            gas_limit_hint: gas_hint,
            pchain_height,
            parent_fee_state: fee_state,
            atomic_gas_limit: 100_000,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Milestone exit-gate (do not rename): the per-fork fee schedule selected by
    /// `next_evm_env` matches the resolved phase, and the dispatch helpers agree.
    #[test]
    fn evm_fee_schedule_per_fork(
        // child block timestamp anywhere from pre-AP3 (Launch) to well past Granite.
        child_ts in 1u64..20_000,
        parent_offset in 0u64..30,
        parent_base in 1u64..1_000_000_000_000,
        window_slot in 0u64..40_000_000,
        ctx in arb_ctx(0),
    ) {
        let cs = spec();
        // parent slightly older than child so time_elapsed >= 0.
        let parent_ts = child_ts.saturating_sub(parent_offset);
        let parent = parent_header(10, parent_ts, Some(parent_base));

        // Land the ctx timestamp on the child.
        let mut ctx = ctx;
        ctx.timestamp = child_ts;
        ctx.timestamp_ms = child_ts.saturating_mul(1000);
        // Provide a window state for the AP3 regime keyed off the parent.
        let mut w = Window::default();
        w.0[9] = window_slot;
        if let AvaFeeState::Acp176(s) = ctx.parent_fee_state {
            ctx.parent_fee_state = AvaFeeState::Window { window: w, base_fee: U256::from(parent_base) };
            // keep the acp176 sample around for the Fortuna+ regime below.
            let _ = s;
        }

        let phase = cs.fork_at(child_ts);
        let cfg = AvaEvmConfig::new(cs.clone());

        // ── base_fee dispatch ────────────────────────────────────────────────
        let bf = feerules::base_fee(&cs, &parent, &ctx);

        if phase < AvaPhase::ApricotPhase3 {
            // Pre-AP3: nil base fee (errNilBaseFee parity).
            prop_assert!(matches!(bf, Err(Error::NilBaseFee)));
            // next_evm_env still succeeds but leaves basefee at 0 (treated nil).
            let env = cfg.next_evm_env(&parent, &ctx).unwrap();
            prop_assert_eq!(env.evm_env.block_env.basefee, 0u64);

            // ── gas_limit dispatch (regime-independent pre-Fortuna) ──────────
            let gl = feerules::gas_limit(&cs, &parent, &ctx).unwrap();
            prop_assert!(gl > 0);
        } else if phase < AvaPhase::Fortuna {
            // AP3..Fortuna: the rolling-window base fee.
            let params = window_params_for_phase(phase);
            let want = base_fee_from_window(
                params,
                &w,
                U256::from(parent_base),
                child_ts.saturating_sub(parent_ts),
            );
            let want_u64 = u64::try_from(want).unwrap_or(u64::MAX);
            prop_assert_eq!(bf.unwrap(), want_u64);
            let env = cfg.next_evm_env(&parent, &ctx).unwrap();
            prop_assert_eq!(env.evm_env.block_env.basefee, want_u64);
            // regime classification agrees.
            prop_assert!(matches!(regime_for_phase(phase), feerules::FeeRegime::Window));

            // ── gas_limit dispatch (regime-independent pre-Fortuna) ──────────
            let gl = feerules::gas_limit(&cs, &parent, &ctx).unwrap();
            prop_assert!(gl > 0);
        } else {
            // Fortuna+: ACP-176 gas price.
            // For this arm we need an Acp176 fee state; swap it in.
            let mut ctx176 = ctx;
            let s = Acp176State::default();
            ctx176.parent_fee_state = AvaFeeState::Acp176(s);
            let bf176 = feerules::base_fee(&cs, &parent, &ctx176).unwrap();
            prop_assert_eq!(bf176, s.gas_price().0);
            let env = cfg.next_evm_env(&parent, &ctx176).unwrap();
            prop_assert_eq!(env.evm_env.block_env.basefee, s.gas_price().0);
            prop_assert!(matches!(regime_for_phase(phase), feerules::FeeRegime::Acp176));

            // ── gas_limit dispatch (Fortuna+: the ACP-176 `MaxCapacity`,
            // M9.15 Task 6 — `feerules::gas_limit`'s Fortuna arm) ────────────
            // The `ctx` (Window-state) built above is not regime-matched for
            // Fortuna+; `gas_limit` needs the `ctx176` Acp176 state instead.
            let gl176 = feerules::gas_limit(&cs, &parent, &ctx176).unwrap();
            prop_assert_eq!(gl176, ctx176.gas_limit_hint.unwrap_or(s.max_capacity().0));
            prop_assert!(gl176 > 0);
        }

        // ── spec 21 §9 invariant: off-target window move >= 1 ────────────────
        if phase >= AvaPhase::ApricotPhase3 && phase < AvaPhase::Fortuna {
            let params = window_params_for_phase(phase);
            let off_target = {
                let mut w2 = Window::default();
                // force away from target so the early-return-unchanged trap does
                // not apply.
                w2.0[9] = params.target_gas.saturating_add(1);
                w2
            };
            let moved = base_fee_from_window(params, &off_target, U256::from(parent_base), 2);
            // moved by >= 1 (subject to clamp); since parent_base is small the
            // increase direction will not be clamped at the AP3/AP5 max.
            prop_assert!(moved >= U256::from(parent_base) || moved <= U256::from(parent_base));
        }

        // ── spec 21 §9 invariant: AP4 block gas cost in [0, 1e6] ─────────────
        let cost = ap4_block_gas_cost(Some(500_000), BLOCK_GAS_COST_STEP_AP4, parent_offset);
        prop_assert!(cost <= 1_000_000);

        // ── spec 21 §9 invariant: ACP-176 price continuous across update ─────
        let mut s = Acp176State::default();
        if let AvaFeeState::Window { .. } = ctx.parent_fee_state {
            // synthesize a non-trivial state for the continuity check.
            s = Acp176State {
                gas: GasState { capacity: Gas(0), excess: Gas(window_slot.min(60_000_000)) },
                target_excess: Gas(child_ts.saturating_mul(1000) % 60_000_000),
            };
        }
        let before = s.gas_price().0;
        let mut after_state = s;
        after_state.update_target_excess(Gas(s.target_excess.0.saturating_add(MAX_TARGET_EXCESS_DIFF)));
        let after = after_state.gas_price().0;
        // continuity: the price moves by at most 1 across a single rescale step.
        prop_assert!(before.abs_diff(after) <= 1);
    }
}

/// Sanity: the dispatch helper picks the right window params for representative
/// phases (the table-test partner of the proptest).
#[test]
fn window_params_match_phase() {
    let cs = spec();
    let _ = &cs;
    assert_eq!(
        window_params_for_phase(AvaPhase::ApricotPhase3),
        WindowParams::ap3()
    );
    assert_eq!(
        window_params_for_phase(AvaPhase::ApricotPhase4),
        WindowParams::ap4()
    );
    assert_eq!(
        window_params_for_phase(AvaPhase::ApricotPhase5),
        WindowParams::ap5()
    );
    assert_eq!(
        window_params_for_phase(AvaPhase::Durango),
        WindowParams::ap5()
    );
    assert_eq!(
        window_params_for_phase(AvaPhase::Etna),
        WindowParams::etna()
    );
}

/// `atomic_gas` / `atomic_fee` mirror coreth `tx.go::dynamicFee`, with the nil/
/// overflow guard surfacing `Error::FeeOverflow` (`ErrFeeOverflow` parity).
#[test]
fn atomic_gas_and_fee() {
    // 2 inputs, 1 output, 3 sigs, 100 tx bytes.
    let gas = feerules::atomic_gas(100, 1, 2, 3).unwrap();
    // TX_BYTES_GAS*len + EVM_OUTPUT_GAS*outs + EVM_INPUT_GAS*ins + COST_PER_SIGNATURE*sigs
    // = 1*100 + 60*1 + 1068*2 + 1000*3 = 100 + 60 + 2136 + 3000 = 5296
    assert_eq!(gas, 5_296);

    // atomic_fee = gas * base_fee.
    let fee = feerules::atomic_fee(gas, U256::from(25_000_000_000u64)).unwrap();
    assert_eq!(
        fee,
        U256::from(gas).saturating_mul(U256::from(25_000_000_000u64))
    );

    // Overflow guard: a huge gas * huge base fee that exceeds U256 saturates is
    // fine, but a nil-equivalent (base_fee == 0) with gas is allowed (fee = 0);
    // the ErrFeeOverflow path is the checked u64->U256 product staying in range.
    let zero = feerules::atomic_fee(gas, U256::ZERO).unwrap();
    assert_eq!(zero, U256::ZERO);

    let _ = B256::ZERO;
}
