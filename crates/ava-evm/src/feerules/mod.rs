// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-fork dynamic fee rules: AP3 base-fee window, AP4 block gas cost,
//! Fortuna/ACP-176, ACP-226 (G2, spec 10 §7, spec 21). Populated by
//! M6.11/M6.12/M6.13.
//!
//! The shared EIP-4844 exponential (`CalculatePrice`, spec 21 §0) is owned by
//! `ava-vm`'s ACP-103 gas primitive; it is the same algorithm AP3/Fortuna
//! route through, so it is re-exported here rather than re-derived.

pub mod acp176;
pub mod acp226;
pub mod blockgas;
pub mod window;

use ruint::aliases::U256;

use crate::atomic::tx::{COST_PER_SIGNATURE, EVM_INPUT_GAS, EVM_OUTPUT_GAS, TX_BYTES_GAS};
use crate::block::AvaHeader;
use crate::chainspec::{AvaChainSpec, AvaPhase};
use crate::error::Error;
use crate::evmconfig::{AvaFeeState, AvaNextBlockCtx};
use crate::feerules::acp176::Acp176State;
use crate::feerules::acp226::{DelayExcess, INITIAL_DELAY_EXCESS};
use crate::feerules::blockgas::{
    BLOCK_GAS_COST_STEP_AP4, BLOCK_GAS_COST_STEP_AP5, ap4_block_gas_cost,
};
use crate::feerules::window::{INTRINSIC_BLOCK_GAS, Window};

// Spec 21 §0: re-export the shared exponential + gas state from the canonical
// owner (`ava_vm::components::gas`) so EVM fee code names one implementation.
pub use ava_vm::components::gas::{Gas, GasState, Price, calculate_price};

// ─── Fork dispatch (spec 21 §7, spec 10 §7.2/§17.3 G2) ────────────────────────

/// Which dynamic-fee regime is active for a resolved [`AvaPhase`] (spec 21 §7).
///
/// The three regimes are mutually exclusive and chronologically ordered:
/// `Legacy` (pre-AP3, no base fee) → `Window` (AP3..Fortuna rolling window) →
/// `Acp176` (Fortuna+ gas-price state machine).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FeeRegime {
    /// Pre-Apricot-Phase-3: legacy pricing, **no** EIP-1559 base fee
    /// (`errNilBaseFee` parity — see [`base_fee`]).
    Legacy,
    /// Apricot Phase 3 through (excluding) Fortuna: the rolling-window base fee
    /// ([`window::base_fee_from_window`], M6.11).
    Window,
    /// Fortuna and later: the ACP-176 gas-price state machine
    /// ([`acp176::Acp176State::gas_price`], M6.12).
    Acp176,
}

/// Resolves the active [`FeeRegime`] for an [`AvaPhase`] (spec 21 §7 dispatch).
#[must_use]
pub fn regime_for_phase(phase: AvaPhase) -> FeeRegime {
    if phase < AvaPhase::ApricotPhase3 {
        FeeRegime::Legacy
    } else if phase < AvaPhase::Fortuna {
        FeeRegime::Window
    } else {
        FeeRegime::Acp176
    }
}

/// Selects the AP3-window [`window::BaseFeeParams`] (target/denominator/bounds)
/// for a resolved phase — coreth keys these off `parent.Time` (spec 21 §4a trap
/// 6 / §7): AP3 → `ap3`, AP4 → `ap4`, AP5..Etna(excl) → `ap5`, Etna+ → `etna`.
///
/// Only meaningful in the [`FeeRegime::Window`] regime; the Fortuna+ ACP-176
/// regime ignores it.
#[must_use]
pub fn window_params_for_phase(phase: AvaPhase) -> window::BaseFeeParams {
    if phase >= AvaPhase::Etna {
        window::BaseFeeParams::etna()
    } else if phase >= AvaPhase::ApricotPhase5 {
        window::BaseFeeParams::ap5()
    } else if phase >= AvaPhase::ApricotPhase4 {
        window::BaseFeeParams::ap4()
    } else {
        window::BaseFeeParams::ap3()
    }
}

/// `base_fee` — the per-fork base-fee dispatch (spec 10 §7.2/§17.3, spec 21 §7).
///
/// Keyed on the phase active at `ctx.timestamp`:
/// - **pre-AP3** ([`FeeRegime::Legacy`]) → [`Error::NilBaseFee`] (coreth
///   `errNilBaseFee`: legacy pricing has no base fee; the caller treats this as
///   "absent" and leaves `block_env.basefee == 0`).
/// - **AP3..Fortuna** ([`FeeRegime::Window`]) → [`window::base_fee_from_window`]
///   over the parent window + parent base fee carried in
///   [`AvaFeeState::Window`]. The window/base-fee come from the parent header's
///   extra-data, extracted by the builder/verifier (M6.7) into the ctx.
/// - **Fortuna+** ([`FeeRegime::Acp176`]) → coreth
///   `feeStateBeforeBlock(parent, childTimeMS).GasPrice()` (`customheader/base_fee.go:27-33`):
///   the parent's ACP-176 state, re-derived from `parent.extra`, is advanced by
///   the elapsed time FIRST and only then read.
///
/// The window arm returns a `u64` (C-Chain base fees fit in `u64`); a value that
/// exceeds `u64::MAX` saturates (it would already be clamped to a phase bound
/// well below that).
///
/// # Errors
/// Returns [`Error::NilBaseFee`] pre-AP3, or if the carried fee-state does not
/// match the active regime (a programming error in the builder wiring).
pub fn base_fee(
    cs: &AvaChainSpec,
    parent: &AvaHeader,
    ctx: &AvaNextBlockCtx,
) -> Result<u64, Error> {
    let phase = cs.fork_at(ctx.timestamp);
    match regime_for_phase(phase) {
        FeeRegime::Legacy => Err(Error::NilBaseFee),
        FeeRegime::Window => {
            let params = window_params_for_phase(phase);
            let (window, parent_base) = match &ctx.parent_fee_state {
                AvaFeeState::Window { window, base_fee } => (*window, *base_fee),
                // Builder wiring error: window regime requires a window state.
                AvaFeeState::Acp176(_) => return Err(Error::NilBaseFee),
            };
            // `time_elapsed = child.Time - parent.Time` (seconds), floored at 0.
            let time_elapsed = ctx.timestamp.saturating_sub(parent.time);
            let bf = window::base_fee_from_window(params, &window, parent_base, time_elapsed);
            Ok(u64::try_from(bf).unwrap_or(u64::MAX))
        }
        // coreth `customheader/base_fee.go:27-33` — the child base fee is
        // `feeStateBeforeBlock(parent, childTimeMS).GasPrice()`: advance the
        // parent state by the elapsed time FIRST (ms at Granite, s at Fortuna),
        // then read the price. Re-derives from `parent.extra` (Go-exact);
        // `ctx.parent_fee_state` is not consulted on this arm.
        FeeRegime::Acp176 => Ok(fee_state_before_block(cs, parent, ctx.timestamp_ms)?
            .gas_price()
            .0),
    }
}

/// `gas_limit` — the per-fork block gas-limit dispatch (spec 10 §7.2/§17.3).
///
/// Pre-Fortuna coreth uses a static ceiling: `ApricotPhase1GasLimit` pre-Cortina,
/// `CortinaGasLimit` from Cortina on (`customheader/gas_limit.go:29-58`
/// `GasLimit`). At Fortuna+ the header `GasLimit` IS the ACP-176 dynamic
/// `MaxCapacity()` off the parent fee state — coreth does NOT keep it fixed
/// there (`gas_limit.go:36-45`). The builder may override via
/// `ctx.gas_limit_hint` in every regime.
///
/// # Errors
/// Returns [`Error::NilBaseFee`] if `ctx.parent_fee_state` carries a `Window`
/// state at Fortuna+ (a builder wiring error: [`parent_fee_state_of`] always
/// resolves the regime matching `cs.fork_at`, so this is unreachable for a
/// correctly wired caller — mirrors [`base_fee`]'s identical regime-mismatch
/// convention above).
pub fn gas_limit(
    cs: &AvaChainSpec,
    _parent: &AvaHeader,
    ctx: &AvaNextBlockCtx,
) -> Result<u64, Error> {
    let phase = cs.fork_at(ctx.timestamp);
    if phase >= AvaPhase::Fortuna {
        // coreth `customheader/gas_limit.go:36-45` (`GasLimit`, Fortuna arm):
        // `state.MaxCapacity()` off the (pre-block) parent fee state. Unlike
        // `base_fee` above, there is no elapsed-time-advance nuance to defer:
        // `MaxCapacity` depends only on `TargetExcess`, which
        // `feeStateBeforeBlock`'s time-advance never touches (only
        // `Gas.{Capacity,Excess}` change — `acp176.rs::AdvanceSeconds`), so
        // reading the raw `ctx.parent_fee_state` gives the byte-exact value.
        return match &ctx.parent_fee_state {
            AvaFeeState::Acp176(state) => Ok(ctx.gas_limit_hint.unwrap_or(state.max_capacity().0)),
            AvaFeeState::Window { .. } => Err(Error::NilBaseFee),
        };
    }
    let default_limit = if phase >= AvaPhase::Cortina {
        CORTINA_GAS_LIMIT
    } else {
        APRICOT_PHASE1_GAS_LIMIT
    };
    Ok(ctx.gas_limit_hint.unwrap_or(default_limit))
}

/// coreth `plugin/evm/upgrade/ap1/params.go:21` — the ApricotPhase1 static gas
/// limit (`ap1.GasLimit`).
pub const APRICOT_PHASE1_GAS_LIMIT: u64 = 8_000_000;
/// coreth `plugin/evm/upgrade/cortina/params.go:11` — the Cortina static gas
/// limit (`cortina.GasLimit`).
pub const CORTINA_GAS_LIMIT: u64 = 15_000_000;
/// coreth `plugin/evm/upgrade/ap0/params.go:27-28` — the pre-AP1 launch range.
pub const AP0_MIN_GAS_LIMIT: u64 = 5_000;
pub const AP0_MAX_GAS_LIMIT: u64 = 0x7fff_ffff_ffff_ffff;
/// coreth `plugin/evm/upgrade/ap0/params.go:29` — the pre-AP1 gas-limit bound
/// divisor (`ap0.GasLimitBoundDivisor`), used by [`verify_gas_limit`]'s pre-AP1
/// bound-divisor arm (`customheader/gas_limit.go:147-157`).
pub const AP0_GAS_LIMIT_BOUND_DIVISOR: u64 = 1024;

/// coreth `customheader/gas_limit.go:101-160` — `VerifyGasLimit`.
///
/// The verify-side complement of [`gas_limit`]: recomputes the expected gas
/// limit from the parent and equality-checks the header's claim (range- and
/// bound-divisor-checks pre-AP1). At Fortuna+ the expectation is the ACP-176
/// `MaxCapacity()` off the time-advanced pre-block state (`gas_limit.go:107-120`).
///
/// # Errors
/// [`Error::GasLimitMismatch`] (Fortuna) / [`Error::GasLimitMismatchInFork`]
/// (Cortina/ApricotPhase1) / [`Error::GasLimitOutOfRange`] /
/// [`Error::GasLimitOutOfBound`] (pre-AP1) on a wrong claim; propagates
/// [`Error::InvalidFeeState`] from the fee-state recompute.
pub fn verify_gas_limit(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let phase = spec.fork_at(header.time);
    if phase >= AvaPhase::Fortuna {
        // gas_limit.go:107-120
        let state = fee_state_before_block(spec, parent, header_time_ms(header))?;
        let want = state.max_capacity().0;
        if header.gas_limit != want {
            return Err(Error::GasLimitMismatch {
                have: header.gas_limit,
                want,
            });
        }
    } else if phase >= AvaPhase::Cortina {
        // gas_limit.go:121-128
        if header.gas_limit != CORTINA_GAS_LIMIT {
            return Err(Error::GasLimitMismatchInFork {
                fork: "Cortina",
                have: header.gas_limit,
                want: CORTINA_GAS_LIMIT,
            });
        }
    } else if phase >= AvaPhase::ApricotPhase1 {
        // gas_limit.go:129-136
        if header.gas_limit != APRICOT_PHASE1_GAS_LIMIT {
            return Err(Error::GasLimitMismatchInFork {
                fork: "ApricotPhase1",
                have: header.gas_limit,
                want: APRICOT_PHASE1_GAS_LIMIT,
            });
        }
    } else {
        // gas_limit.go:138-145
        if header.gas_limit < AP0_MIN_GAS_LIMIT || header.gas_limit > AP0_MAX_GAS_LIMIT {
            return Err(Error::GasLimitOutOfRange {
                have: header.gas_limit,
                min: AP0_MIN_GAS_LIMIT,
                max: AP0_MAX_GAS_LIMIT,
            });
        }
        // gas_limit.go:147-157 — the gas limit may not jump by more than
        // parent.GasLimit / GasLimitBoundDivisor from the parent's.
        let diff = parent.gas_limit.abs_diff(header.gas_limit);
        let limit = parent.gas_limit / AP0_GAS_LIMIT_BOUND_DIVISOR;
        if diff >= limit {
            return Err(Error::GasLimitOutOfBound {
                have: header.gas_limit,
                want: parent.gas_limit,
                limit,
            });
        }
    }
    Ok(())
}

/// coreth `customheader/gas_limit.go:164-180` — `GasCapacity`.
///
/// Pre-Fortuna the capacity IS the gas limit (`GasLimit`, gas_limit.go:30-58:
/// Cortina 15M / AP1 8M / pre-AP1 the parent's own limit); Fortuna+ it is the
/// ACP-176 pre-block state's capacity (`feeStateBeforeBlock`).
///
/// # Errors
/// Propagates [`Error::InvalidFeeState`] from the fee-state recompute.
pub fn gas_capacity(spec: &AvaChainSpec, parent: &AvaHeader, time_ms: u64) -> Result<u64, Error> {
    // gas_limit.go:169.
    let timestamp = time_ms / 1000;
    let phase = spec.fork_at(timestamp);
    if phase >= AvaPhase::Fortuna {
        // gas_limit.go:175-179.
        let state = fee_state_before_block(spec, parent, time_ms)?;
        return Ok(state.gas.capacity.0);
    }
    // gas_limit.go:170-173 → GasLimit's static arms.
    if phase >= AvaPhase::Cortina {
        Ok(CORTINA_GAS_LIMIT)
    } else if phase >= AvaPhase::ApricotPhase1 {
        Ok(APRICOT_PHASE1_GAS_LIMIT)
    } else {
        // gas_limit.go:52-57 — pre-AP1 falls back to the parent's limit.
        Ok(parent.gas_limit)
    }
}

/// coreth `customheader/gas_limit.go:61-98` — `VerifyGasUsed`.
///
/// The claimed `GasUsed` (plus `ExtDataGasUsed` at Fortuna+, when present)
/// must fit within the block's gas capacity. This is the pre-execution
/// capacity bound Go runs inside `verifyIntrinsicGas` (`wrapped_block.go:302`).
///
/// # Errors
/// [`Error::ExtDataGasUsedTooLarge`] (non-u64 claim) / [`Error::FeeOverflow`]
/// (u64 overflow of the sum) / [`Error::GasUsedOverCapacity`]; propagates
/// [`gas_capacity`]'s errors.
pub fn verify_gas_used(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let mut gas_used = header.gas_used;
    // gas_limit.go:69-82 — fold ExtDataGasUsed in at Fortuna+.
    if spec.is_fortuna(header.time)
        && let Some(ext) = header.ext_data_gas_used
    {
        let ext_u64 = u64::try_from(ext).map_err(|_| Error::ExtDataGasUsedTooLarge(ext))?;
        gas_used = gas_used.checked_add(ext_u64).ok_or(Error::FeeOverflow)?;
    }
    // gas_limit.go:84-96.
    let capacity = gas_capacity(spec, parent, header_time_ms(header))?;
    if gas_used > capacity {
        return Err(Error::GasUsedOverCapacity {
            have: gas_used,
            capacity,
        });
    }
    Ok(())
}

/// coreth `customheader/time.go:20` — `MaxFutureBlockTime` (10 s), in ms.
pub const MAX_FUTURE_BLOCK_TIME_MS: u64 = 10_000;

/// coreth `customheader/time.go:55-124` — `VerifyTime`.
///
/// Verifies the header's `Time`/`TimeMilliseconds` against the parent, the
/// rules, and the current time `now_ms` (Go passes `b.vm.clock.Time()`):
/// non-decreasing vs parent (equality allowed), not beyond `now + 10s`,
/// `TimeMilliseconds` nil pre-Granite / required + consistent at Granite, and
/// the ACP-226 minimum block delay demanded by the PARENT's `MinDelayExcess`.
///
/// # Errors
/// [`Error::BlockTooOld`] / [`Error::BlockTooFarInFuture`] /
/// [`Error::TimeMillisecondsBeforeGranite`] / [`Error::TimeMillisecondsRequired`] /
/// [`Error::TimeMillisecondsMismatched`] / [`Error::MinDelayNotMet`].
pub fn verify_time(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
    now_ms: u64,
) -> Result<(), Error> {
    // time.go:62-63 — both sides through the HeaderTimeMilliseconds fallback.
    let header_ms = header_time_ms(header);
    let parent_ms = header_time_ms(parent);

    // time.go:65-70 — non-decreasing; equality allowed.
    if header_ms < parent_ms {
        return Err(Error::BlockTooOld {
            have: header_ms,
            parent: parent_ms,
        });
    }

    // time.go:72-79 — future bound.
    let max_ms = now_ms.saturating_add(MAX_FUTURE_BLOCK_TIME_MS);
    if header_ms > max_ms {
        return Err(Error::BlockTooFarInFuture {
            have: header_ms,
            allowed: max_ms,
        });
    }

    // time.go:81-87 — pre-Granite: the field must be absent.
    if !spec.is_granite(header.time) {
        if header.time_milliseconds.is_some() {
            return Err(Error::TimeMillisecondsBeforeGranite);
        }
        return Ok(());
    }

    // time.go:89-92 — Granite: required.
    let Some(ms) = header.time_milliseconds else {
        return Err(Error::TimeMillisecondsRequired);
    };

    // time.go:94-101 — Time == TimeMilliseconds/1000.
    let expected_time = ms / 1000;
    if header.time != expected_time {
        return Err(Error::TimeMillisecondsMismatched {
            time: header.time,
            expected: expected_time,
        });
    }

    // time.go:103-108 — a parent without an excess (the first Granite block)
    // cannot demand a delay.
    let Some(parent_excess) = parent.min_delay_excess else {
        return Ok(());
    };

    // time.go:110-121 — the ordering check above proved header_ms >= parent_ms,
    // so the subtraction cannot underflow (Go carries the same comment).
    let actual = header_ms.saturating_sub(parent_ms);
    let required = DelayExcess(parent_excess).delay();
    if actual < required {
        return Err(Error::MinDelayNotMet { actual, required });
    }
    Ok(())
}

// ─── ACP-176 fee-state extra prefix + parent-fee-state plumbing (spec 21 §5) ──
//
// Port of coreth `plugin/evm/customheader/{extra,dynamic_fee_state,
// dynamic_fee_windower,min_delay_excess}.go`. Every state transition below
// cites the exact Go line it mirrors and applies the primitives in Go's order
// (`parse parent → advance time → consume gas → optional target-excess step`).

/// coreth `plugin/evm/customtypes/header_ext.go:59` — `HeaderTimeMilliseconds`.
/// A header with an explicit `TimeMilliseconds` (Granite+) reports it directly;
/// otherwise the second-granularity `Time × 1000` is used.
#[must_use]
pub(crate) fn header_time_ms(h: &AvaHeader) -> u64 {
    match h.time_milliseconds {
        Some(ms) => ms,
        None => h.time.saturating_mul(1000),
    }
}

/// Narrows an optional `U256` header field to `u64` (ext-data-gas / block-gas
/// values are bounded well below `u64::MAX`; an out-of-range value saturates —
/// the same convention `builder.rs::u256_to_u64` uses).
#[must_use]
fn opt_u256_to_u64(v: Option<U256>) -> u64 {
    v.map(|x| u64::try_from(x).unwrap_or(u64::MAX)).unwrap_or(0)
}

/// coreth `customheader/dynamic_fee_state.go:18` — `feeStateBeforeBlock`.
///
/// Computes the ACP-176 fee state *before* the child block's own gas is
/// consumed: parse the parent's state (or seed zero for a genesis / pre-Fortuna
/// parent), then advance it by the elapsed time. `time_ms` is the child's
/// millisecond timestamp (`HeaderTimeMilliseconds(header)`).
///
/// # Errors
/// Returns [`Error::InvalidFeeState`] if the child timestamp precedes the
/// parent's (`errInvalidTimestamp`) or the parent extra prefix is malformed.
pub fn fee_state_before_block(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    time_ms: u64,
) -> Result<Acp176State, Error> {
    // coreth dynamic_fee_state.go:24
    let timestamp = time_ms / 1000;
    // coreth dynamic_fee_state.go:25
    let parent_ms = header_time_ms(parent);
    // coreth dynamic_fee_state.go:26-32
    if time_ms < parent_ms {
        return Err(Error::InvalidFeeState(format!(
            "timestamp {timestamp} prior to parent timestamp {}",
            parent.time
        )));
    }

    // coreth dynamic_fee_state.go:34-44 — a pre-Fortuna or genesis (number 0)
    // parent seeds the zero state; otherwise the parent's verified fee state is
    // the starting point.
    let mut state = Acp176State::default();
    if spec.is_fortuna(parent.time) && parent.number != 0 {
        state = Acp176State::from_bytes(&parent.extra)
            .map_err(|e| Error::InvalidFeeState(format!("parsing parent fee state: {e}")))?;
    }

    // coreth dynamic_fee_state.go:46-51 — Granite advances at millisecond
    // granularity; Fortuna (pre-Granite) at whole seconds. `saturating_sub` is
    // exact here (the guard above proved `time_ms >= parent_ms`, and coreth only
    // reaches the Fortuna arm when `timestamp >= parent.Time`).
    if spec.is_granite(timestamp) {
        state.advance_milliseconds(time_ms.saturating_sub(parent_ms));
    } else if spec.is_fortuna(timestamp) {
        state.advance_seconds(timestamp.saturating_sub(parent.time));
    }
    Ok(state)
}

/// coreth `customheader/dynamic_fee_state.go:57` — `feeStateAfterBlock`.
///
/// The ACP-176 fee state *after* the child block executes: the pre-block state
/// ([`fee_state_before_block`]) with the child's gas consumed, then an optional
/// move of the target excess toward `desired_target_excess`. Its [`Acp176State::to_bytes`]
/// is exactly the header extra prefix coreth `ExtraPrefix` stamps at Fortuna+
/// (`extra.go:36-44`).
///
/// `time`/`time_ms` are the child header's `Time`/`TimeMilliseconds`; `gas_used`
/// is the EVM gas used; `ext_data_gas_used` is the atomic (ExtData) gas used.
/// A `desired_target_excess` of `None` leaves the parent's target excess intact
/// (coreth passes `nil` when no `GasTarget` is configured — the default).
///
/// # Errors
/// Propagates [`fee_state_before_block`]'s errors, or [`Error::FeeOverflow`] if
/// the block's gas exceeds the available capacity (`gas.ErrInsufficientCapacity`).
pub fn fee_state_after_block(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    time: u64,
    time_ms: Option<u64>,
    gas_used: u64,
    ext_data_gas_used: u64,
    desired_target_excess: Option<u64>,
) -> Result<Acp176State, Error> {
    // coreth dynamic_fee_state.go:64 — timeMS := HeaderTimeMilliseconds(header).
    let time_ms_val = time_ms.unwrap_or(time.saturating_mul(1000));
    // coreth dynamic_fee_state.go:65
    let mut state = fee_state_before_block(spec, parent, time_ms_val)?;
    // coreth dynamic_fee_state.go:70-73 — consume the block's EVM + ExtData gas.
    state.consume_gas(gas_used, Some(u128::from(ext_data_gas_used)))?;
    // coreth dynamic_fee_state.go:77-79 — move the target excess toward the
    // desired value (skipped entirely when nil).
    if let Some(desired) = desired_target_excess {
        state.update_target_excess(Gas(desired));
    }
    Ok(state)
}

/// coreth `customheader/dynamic_fee_windower.go:148` — `feeWindow`.
///
/// The AP3-regime rolling gas window *after* the parent block, used as the
/// header extra prefix in the `[ApricotPhase3, Fortuna)` regime. Parses the
/// parent window, folds in the parent's consumed gas (EVM + ExtData +
/// blockGasCost, per the parent's phase), and shifts by the elapsed time.
///
/// # Errors
/// Returns [`Error::InvalidFeeState`] if the parent extra prefix is too short to
/// hold a window, or the child timestamp precedes the parent's.
pub fn fee_window(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    timestamp: u64,
) -> Result<Window, Error> {
    // coreth dynamic_fee_windower.go:156-158 — first EIP-1559 block or genesis.
    if !spec.is_apricot_phase3(parent.time) || parent.number == 0 {
        return Ok(Window::default());
    }
    // coreth dynamic_fee_windower.go:160
    let mut window = Window::from_bytes(&parent.extra)
        .ok_or_else(|| Error::InvalidFeeState("parsing parent fee window".to_string()))?;
    // coreth dynamic_fee_windower.go:164-170
    if timestamp < parent.time {
        return Err(Error::InvalidFeeState(format!(
            "timestamp {timestamp} prior to parent timestamp {}",
            parent.time
        )));
    }
    // coreth dynamic_fee_windower.go:171
    let time_elapsed = timestamp.saturating_sub(parent.time);

    // coreth dynamic_fee_windower.go:174-208 — the parent's consumed gas depends
    // on the parent's phase.
    let (block_gas_cost, parent_ext_gas_used) = if spec.is_apricot_phase5(parent.time) {
        // dynamic_fee_windower.go:176-182 — blockGasCost is 0 after AP5.
        (0, opt_u256_to_u64(parent.ext_data_gas_used))
    } else if spec.is_apricot_phase4(parent.time) {
        // dynamic_fee_windower.go:183-204 — AP4 uses the AP4 step (even if the
        // child is AP5), preserving the original coreth behaviour.
        (
            ap4_block_gas_cost(
                parent
                    .block_gas_cost
                    .map(|c| u64::try_from(c).unwrap_or(u64::MAX)),
                BLOCK_GAS_COST_STEP_AP4,
                time_elapsed,
            ),
            opt_u256_to_u64(parent.ext_data_gas_used),
        )
    } else {
        // dynamic_fee_windower.go:205-207 — AP3 folds in the intrinsic block gas.
        (INTRINSIC_BLOCK_GAS, 0)
    };

    // coreth dynamic_fee_windower.go:211
    window.add(&[parent.gas_used, parent_ext_gas_used, block_gas_cost]);
    // coreth dynamic_fee_windower.go:215
    window.shift(time_elapsed);
    Ok(window)
}

/// coreth `customheader/extra.go:30` — `ExtraPrefix`.
///
/// The exact bytes the child header's `Extra` must open with, keyed on the phase
/// active at the child's `time`: the 24-byte ACP-176 fee state at Fortuna+
/// ([`fee_state_after_block`]), the 80-byte AP3 window in `[AP3, Fortuna)`
/// ([`fee_window`]), or empty pre-AP3.
///
/// # Errors
/// Propagates the fee-state / fee-window transition errors.
#[allow(clippy::too_many_arguments)]
pub fn extra_prefix(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    time: u64,
    time_ms: Option<u64>,
    gas_used: u64,
    ext_data_gas_used: u64,
    desired_target_excess: Option<u64>,
) -> Result<Vec<u8>, Error> {
    // coreth extra.go:37-58 — keyed on the CHILD header time (`header.Time`).
    let phase = spec.fork_at(time);
    if phase >= AvaPhase::Fortuna {
        Ok(fee_state_after_block(
            spec,
            parent,
            time,
            time_ms,
            gas_used,
            ext_data_gas_used,
            desired_target_excess,
        )?
        .to_bytes()
        .to_vec())
    } else if phase >= AvaPhase::ApricotPhase3 {
        Ok(fee_window(spec, parent, time)?.to_bytes().to_vec())
    } else {
        // extra.go:54-56 — prior to AP3 there is no expected extra prefix.
        Ok(Vec::new())
    }
}

/// coreth `customheader/extra.go:62-111` — `VerifyExtraPrefix`.
///
/// Fortuna+: the header's claimed ACP-176 fee state (first 24 bytes of
/// `Extra`) must equal `feeStateAfterBlock(parent, header, claimed.
/// TargetExcess)` — the claimed target excess is passed as the desired value
/// so the expectation clamps toward the claim (`extra.go:74-87`); a claim
/// reachable in one step therefore matches exactly, anything else mismatches.
/// `[AP3, Fortuna)`: `Extra` must start with the recomputed fee window's
/// bytes. Pre-AP3: no expected prefix.
///
/// `VerifyExtraPrefix` has no Helicon arm — the Fortuna check still runs
/// under Helicon (forks are cumulative, so `IsFortuna` stays true). The
/// `IsHelicon` short-circuit (`return nil`) belongs only to the sibling
/// `VerifyExtra` (`extra.go:120-121`), ported separately in
/// `EvmBlock::syntactic_verify` (`block.rs`, its own upstream-delta callout);
/// add no Helicon handling to this function.
///
/// # Errors
/// [`Error::IncorrectFeeState`] / [`Error::InvalidExtraPrefix`] on mismatch;
/// [`Error::InvalidFeeState`] if the claimed or parent state is unparsable.
pub fn verify_extra_prefix(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    let phase = spec.fork_at(header.time);
    if phase >= AvaPhase::Fortuna {
        // extra.go:69-72 — parse the CLAIMED fee state off the header.
        let claimed = Acp176State::from_bytes(&header.extra)
            .map_err(|e| Error::InvalidFeeState(format!("parsing remote fee state: {e}")))?;
        // extra.go:74-87
        let expected = fee_state_after_block(
            spec,
            parent,
            header.time,
            header.time_milliseconds,
            header.gas_used,
            opt_u256_to_u64(header.ext_data_gas_used),
            Some(claimed.target_excess.0),
        )?;
        // extra.go:89-95
        if claimed != expected {
            return Err(Error::IncorrectFeeState {
                expected: format!("{expected:?}"),
                found: format!("{claimed:?}"),
            });
        }
    } else if phase >= AvaPhase::ApricotPhase3 {
        // extra.go:96-108
        let window = fee_window(spec, parent, header.time)?;
        let want = window.to_bytes();
        if !header.extra.starts_with(want.as_slice()) {
            return Err(Error::InvalidExtraPrefix {
                expected: hex::encode(want),
                found: hex::encode(&header.extra),
            });
        }
    }
    Ok(())
}

/// The parent's dynamic-fee state, parsed from its header extra prefix, to
/// thread into the child's [`AvaNextBlockCtx::parent_fee_state`] so
/// [`base_fee`]/[`AvaEvmConfig::next_evm_env`](crate::evmconfig::AvaEvmConfig)
/// derive the child base fee from the real parent state (M6.7).
///
/// Mirrors the initial-state selection coreth makes in `feeStateBeforeBlock`
/// (`dynamic_fee_state.go:34-44`) and `feeWindow` (`dynamic_fee_windower.go:156-160`):
/// a genesis / pre-regime parent seeds the empty state, otherwise the parent's
/// own extra prefix is parsed.
///
/// # Errors
/// Returns [`Error::InvalidFeeState`] if a non-genesis parent's extra prefix is
/// malformed for its regime.
pub fn parent_fee_state_of(spec: &AvaChainSpec, parent: &AvaHeader) -> Result<AvaFeeState, Error> {
    match regime_for_phase(spec.fork_at(parent.time)) {
        FeeRegime::Acp176 => {
            // dynamic_fee_state.go:35 — `IsFortuna(parent.Time) && number != 0`
            // (the phase match already established `IsFortuna(parent.Time)`).
            let state = if parent.number != 0 {
                Acp176State::from_bytes(&parent.extra)
                    .map_err(|e| Error::InvalidFeeState(format!("parsing parent fee state: {e}")))?
            } else {
                Acp176State::default()
            };
            Ok(AvaFeeState::Acp176(state))
        }
        FeeRegime::Window => {
            // dynamic_fee_windower.go:156-160 — the raw parent window (empty for
            // the first AP3 block / genesis) plus the parent's base fee.
            let window = if parent.number != 0 && spec.is_apricot_phase3(parent.time) {
                Window::from_bytes(&parent.extra).ok_or_else(|| {
                    Error::InvalidFeeState("parsing parent fee window".to_string())
                })?
            } else {
                Window::default()
            };
            Ok(AvaFeeState::Window {
                window,
                base_fee: parent.base_fee.unwrap_or(U256::ZERO),
            })
        }
        // Pre-AP3: legacy pricing has no fee state (base_fee dispatch returns
        // `NilBaseFee` regardless of this value).
        FeeRegime::Legacy => Ok(AvaFeeState::default()),
    }
}

// ─── verifyHeaderGasFields (spec 21 §7 verify path) ────────────────────────
//
// Port of coreth `consensus/dummy/consensus.go:125-176` + the
// `customheader/block_gas_cost.go:31-59` wrapper it calls into.

/// coreth `customheader/block_gas_cost.go:31-59` — `BlockGasCost`, the
/// fork-gated wrapper over [`blockgas::block_gas_cost`]: `None` pre-AP4,
/// `Some(0)` at Granite, else the AP4/AP5-stepped cost off the parent.
#[must_use]
pub fn expected_block_gas_cost(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    timestamp: u64,
) -> Option<u64> {
    // block_gas_cost.go:36-38
    if !spec.is_apricot_phase4(timestamp) {
        return None;
    }
    // block_gas_cost.go:42-45
    let step = if spec.is_apricot_phase5(timestamp) {
        BLOCK_GAS_COST_STEP_AP5
    } else {
        BLOCK_GAS_COST_STEP_AP4
    };
    // block_gas_cost.go:46-53 — an invalid parent/current time combination
    // counts as 0 elapsed time.
    let time_elapsed = timestamp.saturating_sub(parent.time);
    Some(blockgas::block_gas_cost(
        parent
            .block_gas_cost
            .map(|c| u64::try_from(c).unwrap_or(u64::MAX)),
        step,
        time_elapsed,
        spec.is_granite(timestamp),
    ))
}

/// coreth `consensus/dummy/consensus.go:125-176` — `verifyHeaderGasFields`.
///
/// The contextual (parent-dependent) fee/gas equality checks that complement
/// the parent-less structural checks in `EvmBlock::syntactic_verify` — coreth
/// keeps both layers, and so do we. Checks run in Go's order so a multi-fault
/// header reports Go's first rejection class. Go's `VerifyGasUsed` is NOT
/// called here (same comment as consensus.go:126-127): in Go it runs
/// PRE-execution, in `verifyIntrinsicGas` (semantic-verify stage,
/// `wrapped_block.go`) — and that check is UNPORTED (documented follow-up,
/// pre-existing gap). Rust's only gas-used guard is the POST-execution
/// executed-gas equality check in `EvmBlock::verify` (asserts executed gas ==
/// `header.gas_used`) — fail-closed, but later in the pipeline than Go's.
///
/// # Errors
/// The first failing check's error (see the per-check variants); recompute
/// failures propagate as [`Error::InvalidFeeState`] / [`Error::NilBaseFee`].
pub fn verify_header_gas_fields(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    // consensus.go:128-130
    verify_gas_limit(spec, parent, header)?;
    // consensus.go:131-133
    verify_extra_prefix(spec, parent, header)?;

    // consensus.go:136-144 — expected base fee via the SAME `base_fee` the
    // builder stamps with (nil pre-AP3; `utils.BigEqual` treats nil-vs-non-nil
    // as unequal in both directions). Go dispatches off `timeMS`
    // (`customtypes.HeaderTimeMilliseconds(header)`); `header.time` is the
    // same instant at second granularity, so `spec.fork_at(header.time)`
    // resolves the identical phase.
    let phase = spec.fork_at(header.time);
    let expected_base_fee = if phase >= AvaPhase::ApricotPhase3 {
        let ctx = AvaNextBlockCtx {
            timestamp: header.time,
            timestamp_ms: header_time_ms(header),
            parent_fee_state: parent_fee_state_of(spec, parent)?,
            ..AvaNextBlockCtx::default()
        };
        Some(U256::from(base_fee(spec, parent, &ctx)?))
    } else {
        None
    };
    if header.base_fee != expected_base_fee {
        return Err(Error::BaseFeeMismatch {
            expected: expected_base_fee,
            found: header.base_fee,
        });
    }

    // consensus.go:146-156 — BlockGasCost equality (`utils.BigEqual`: nil == nil).
    let want = expected_block_gas_cost(spec, parent, header.time).map(U256::from);
    if header.block_gas_cost != want {
        return Err(Error::BlockGasCostMismatch {
            have: header.block_gas_cost,
            want,
        });
    }

    // consensus.go:158-175 — ExtDataGasUsed fork gating.
    if phase < AvaPhase::ApricotPhase4 {
        if let Some(v) = header.ext_data_gas_used {
            return Err(Error::ExtDataGasUsedBeforeFork(v));
        }
        return Ok(());
    }
    match header.ext_data_gas_used {
        None => Err(Error::NilExtDataGasUsed),
        Some(v) if v > U256::from(u64::MAX) => Err(Error::ExtDataGasUsedTooLarge(v)),
        Some(_) => Ok(()),
    }
}

/// coreth `customheader/min_delay_excess.go:26` — `MinDelayExcess`.
///
/// The child header's ACP-226 min-delay-excess: `None` pre-Granite; at Granite
/// the parent's excess (or [`INITIAL_DELAY_EXCESS`] if the parent pre-dates
/// Granite) moved toward `desired` (coreth passes `nil` — no move — by default).
///
/// # Errors
/// Returns [`Error::InvalidFeeState`] if a Granite parent is missing its
/// min-delay-excess (`errParentMinDelayExcessNil`).
pub fn min_delay_excess_of(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    timestamp: u64,
    desired: Option<DelayExcess>,
) -> Result<Option<u64>, Error> {
    // min_delay_excess.go:33-40 — nil unless the child is in Granite.
    if !spec.is_granite(timestamp) {
        return Ok(None);
    }
    // min_delay_excess.go:86-99 — the inner `minDelayExcess`.
    let mut mde = INITIAL_DELAY_EXCESS;
    if spec.is_granite(parent.time) {
        mde =
            DelayExcess(parent.min_delay_excess.ok_or_else(|| {
                Error::InvalidFeeState("parent min delay excess is nil".to_string())
            })?);
    }
    if let Some(desired) = desired {
        mde.update(desired);
    }
    Ok(Some(mde.0))
}

/// coreth `customheader/min_delay_excess.go:45-81` — `VerifyMinDelayExcess`.
///
/// Granite-only: the header's ACP-226 `MinDelayExcess` must be present and
/// equal the recompute from the parent with the CLAIMED value as the desired
/// target — Go's claimed-as-desired trick (`min_delay_excess.go:59-63`): if the
/// claim was reachable in one update step, the recompute lands exactly on it;
/// otherwise the recompute stops short and the equality fails.
///
/// # Errors
/// [`Error::RemoteMinDelayExcessNil`] / [`Error::IncorrectMinDelayExcess`];
/// propagates [`Error::InvalidFeeState`] from [`min_delay_excess_of`].
pub fn verify_min_delay_excess(
    spec: &AvaChainSpec,
    parent: &AvaHeader,
    header: &AvaHeader,
) -> Result<(), Error> {
    // min_delay_excess.go:50-52.
    if !spec.is_granite(header.time) {
        return Ok(());
    }
    // min_delay_excess.go:54-57.
    let Some(found) = header.min_delay_excess else {
        return Err(Error::RemoteMinDelayExcessNil);
    };
    // min_delay_excess.go:59-71.
    let Some(expected) = min_delay_excess_of(spec, parent, header.time, Some(DelayExcess(found)))?
    else {
        // Unreachable: min_delay_excess_of returns Some whenever the child
        // timestamp is in Granite, which the guard above established.
        return Err(Error::InvalidFeeState(
            "expected min delay excess absent at Granite".to_string(),
        ));
    };
    // min_delay_excess.go:73-79.
    if found != expected {
        return Err(Error::IncorrectMinDelayExcess { expected, found });
    }
    Ok(())
}

// ─── Atomic-tx gas / fee (spec 10 §7.3/§17.3) ─────────────────────────────────

/// `atomic_gas` — the gas an atomic (X<->C) tx consumes (coreth
/// `atomic/tx.go::Gas` / `GasUsed`, spec 10 §7.3/§17.3):
///
/// ```text
/// gas = TxBytesGas*len + EVMOutputGas*outs + EVMInputGas*ins + CostPerSignature*sigs
/// ```
///
/// (`EVMInputGas` already folds in one `CostPerSignature`; `num_signatures` is
/// the **additional** credential signatures beyond the per-input cost — the
/// caller passes the total signature count and we charge `CostPerSignature` per
/// signature, matching coreth's `Complexity`/`Gas` accumulation.)
///
/// All arithmetic is checked (`ErrFeeOverflow` parity, spec 00 §6.1).
///
/// # Errors
/// Returns [`Error::FeeOverflow`] if the accumulation overflows `u64`.
pub fn atomic_gas(
    tx_len: u64,
    num_outputs: u64,
    num_inputs: u64,
    num_signatures: u64,
) -> Result<u64, Error> {
    let bytes_gas = TX_BYTES_GAS.checked_mul(tx_len).ok_or(Error::FeeOverflow)?;
    let output_gas = EVM_OUTPUT_GAS
        .checked_mul(num_outputs)
        .ok_or(Error::FeeOverflow)?;
    let input_gas = EVM_INPUT_GAS
        .checked_mul(num_inputs)
        .ok_or(Error::FeeOverflow)?;
    let sig_gas = COST_PER_SIGNATURE
        .checked_mul(num_signatures)
        .ok_or(Error::FeeOverflow)?;
    bytes_gas
        .checked_add(output_gas)
        .and_then(|g| g.checked_add(input_gas))
        .and_then(|g| g.checked_add(sig_gas))
        .ok_or(Error::FeeOverflow)
}

/// `atomic_fee` — the AVAX fee an atomic tx must pay at the active base fee
/// (coreth `atomic/tx.go::dynamicFee`): `fee = atomic_gas * base_fee`
/// (the `nil baseFee` overflow guard → [`Error::FeeOverflow`], spec 10 §17.3).
///
/// Computed in [`U256`] (base fee is a wei quantity); the product is checked so
/// an overflow surfaces `ErrFeeOverflow` rather than wrapping.
///
/// # Errors
/// Returns [`Error::FeeOverflow`] if `atomic_gas * base_fee` exceeds `U256`.
pub fn atomic_fee(atomic_gas: u64, base_fee: U256) -> Result<U256, Error> {
    U256::from(atomic_gas)
        .checked_mul(base_fee)
        .ok_or(Error::FeeOverflow)
}

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

#[cfg(test)]
mod fee_state_tests {
    use ava_evm_reth::{Address, B256, Bytes, Chain, U256, keccak256};

    use super::{
        AvaFeeState, Gas, GasState, extra_prefix, fee_state_after_block, fee_state_before_block,
        fee_window, min_delay_excess_of, parent_fee_state_of,
    };
    use crate::block::AvaHeader;
    use crate::chainspec::{AvaChainSpec, NetworkUpgrades};
    use crate::error::Error;
    use crate::feerules::acp176::{Acp176State, STATE_SIZE};
    use crate::feerules::acp226::{DelayExcess, INITIAL_DELAY_EXCESS};
    use crate::feerules::window::{WINDOW_SIZE, Window};

    // A schedule with every listed fork active at `from` (else far-future).
    fn spec_from(fortuna: u64, granite: u64, ap3: u64) -> AvaChainSpec {
        const FF: u64 = u64::MAX;
        let upgrades = NetworkUpgrades {
            apricot_phase_1: 0,
            apricot_phase_2: 0,
            apricot_phase_3: ap3,
            apricot_phase_4: ap3,
            apricot_phase_5: ap3,
            apricot_phase_pre_6: ap3,
            apricot_phase_6: ap3,
            apricot_phase_post_6: ap3,
            banff: ap3,
            cortina: ap3,
            durango: ap3,
            etna: fortuna.min(granite),
            fortuna,
            granite,
            helicon: FF,
        };
        AvaChainSpec::from_parts(upgrades, Chain::from_id(43112), false)
    }

    // A minimal header carrying only the fields the fee-state transitions read.
    fn hdr(number: u64, time: u64, time_ms: Option<u64>, extra: Vec<u8>) -> AvaHeader {
        AvaHeader {
            parent_hash: B256::ZERO,
            uncle_hash: B256::ZERO,
            coinbase: Address::ZERO,
            state_root: B256::ZERO,
            tx_root: B256::ZERO,
            receipt_root: B256::ZERO,
            bloom: Bytes::from(vec![0u8; 256]),
            difficulty: U256::ZERO,
            number,
            gas_limit: 15_000_000,
            gas_used: 0,
            time,
            extra: Bytes::from(extra),
            mix_digest: B256::ZERO,
            nonce: [0u8; 8],
            ext_data_hash: keccak256([]),
            base_fee: Some(U256::from(25_000_000_000u64)),
            ext_data_gas_used: None,
            block_gas_cost: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_root: None,
            time_milliseconds: time_ms,
            min_delay_excess: None,
        }
    }

    // ── fee_state_after_block: the live-block-1 golden numbers (Granite) ───────
    // The recorded local block 1 (time 1_784_101_740 / ms 1_784_101_740_277,
    // gas_used 21_000) built on the local genesis (time 1_607_144_400) carries
    // extra prefix capacity=9_979_000, excess=21_000, target_excess=0.
    #[test]
    fn after_block_matches_live_block1_numbers() {
        let spec = spec_from(0, 0, 0); // Granite from genesis (local schedule).
        let parent = hdr(0, 1_607_144_400, Some(1_607_144_400_000), vec![]);
        let got = fee_state_after_block(
            &spec,
            &parent,
            1_784_101_740,
            Some(1_784_101_740_277),
            21_000,
            0,
            None,
        )
        .expect("fee_state_after_block");
        assert_eq!(
            got.gas.capacity,
            Gas(9_979_000),
            "capacity = maxCap - gasUsed"
        );
        assert_eq!(got.gas.excess, Gas(21_000), "excess = gasUsed");
        assert_eq!(
            got.target_excess,
            Gas(0),
            "target excess unchanged (desired nil)"
        );
    }

    // ── fee_state_before_block: Granite advances by ms, Fortuna by seconds ─────
    #[test]
    fn before_block_granite_vs_fortuna_advance() {
        // Granite: advance_milliseconds(child_ms - parent_ms).
        let g = spec_from(0, 0, 0);
        let parent = hdr(0, 100, Some(100_000), vec![]);
        // 1000ms later, from the zero state (target=1M/s ⇒ R=2M/s): capacity fills
        // to 2_000_000 (1s @ R=2M/s), excess stays 0. (advance_milliseconds(1000)
        // == advance_seconds(1).)
        let before = fee_state_before_block(&g, &parent, 101_000).expect("granite before");
        assert_eq!(before.gas.capacity, Gas(2_000_000));
        assert_eq!(before.gas.excess, Gas(0));

        // Fortuna (pre-Granite): whole-second advance — identical for 1s.
        let f = spec_from(0, u64::MAX, 0);
        let before_f = fee_state_before_block(&f, &parent, 101_000).expect("fortuna before");
        assert_eq!(before_f.gas.capacity, Gas(2_000_000));
    }

    // ── fee_state_before_block: child timestamp before parent errors ──────────
    #[test]
    fn before_block_rejects_backwards_time() {
        let g = spec_from(0, 0, 0);
        let parent = hdr(0, 100, Some(100_000), vec![]);
        let err = fee_state_before_block(&g, &parent, 99_999).expect_err("backwards time");
        assert!(matches!(err, Error::InvalidFeeState(_)), "got {err:?}");
    }

    // ── fee_state_before_block: a Fortuna+ non-genesis parent seeds its state ──
    #[test]
    fn before_block_parses_nonzero_parent_state() {
        let g = spec_from(0, 0, 0);
        // Parent (number 1) carries a fee state with a non-zero target excess.
        let parent_state = Acp176State {
            gas: GasState {
                capacity: Gas(5_000_000),
                excess: Gas(0),
            },
            target_excess: Gas(13_605_152), // target = 1_500_000/s
        };
        let parent = hdr(1, 200, Some(200_000), parent_state.to_bytes().to_vec());
        // Zero elapsed: the state is the parent's, unchanged (advance(0) caps cap).
        let before = fee_state_before_block(&g, &parent, 200_000).expect("before");
        assert_eq!(
            before.target_excess,
            Gas(13_605_152),
            "parent target excess parsed"
        );
    }

    // ── extra_prefix: phase-keyed length (Fortuna 24, AP3 80, pre-AP3 empty) ───
    #[test]
    fn extra_prefix_is_phase_keyed() {
        // Fortuna+ (Granite): 24-byte ACP-176 state.
        let g = spec_from(0, 0, 0);
        let parent = hdr(0, 100, Some(100_000), vec![]);
        let p = extra_prefix(&g, &parent, 200, Some(200_000), 21_000, 0, None).expect("granite");
        assert_eq!(p.len(), STATE_SIZE);

        // AP3..<Fortuna (Fortuna far-future): 80-byte fee window.
        let w = spec_from(u64::MAX, u64::MAX, 0);
        let p2 = extra_prefix(&w, &parent, 200, None, 21_000, 0, None).expect("ap3");
        assert_eq!(p2.len(), WINDOW_SIZE);

        // Pre-AP3 (AP3 far-future): empty.
        let l = spec_from(u64::MAX, u64::MAX, u64::MAX);
        let p3 = extra_prefix(&l, &parent, 200, None, 0, 0, None).expect("pre-ap3");
        assert!(p3.is_empty());
    }

    // ── fee_window: genesis / first-AP3 parent => empty window ────────────────
    #[test]
    fn fee_window_empty_on_genesis_parent() {
        let w = spec_from(u64::MAX, u64::MAX, 0);
        let parent = hdr(0, 100, None, vec![]); // number 0
        let win = fee_window(&w, &parent, 200).expect("window");
        assert_eq!(win, Window::default());
    }

    // ── parent_fee_state_of: genesis Fortuna parent => zero ACP-176 state ──────
    #[test]
    fn parent_fee_state_acp176_genesis_is_zero() {
        let g = spec_from(0, 0, 0);
        let genesis = hdr(0, 100, Some(100_000), vec![]);
        let state = parent_fee_state_of(&g, &genesis).expect("parent fee state");
        assert_eq!(state, AvaFeeState::Acp176(Acp176State::default()));
    }

    // ── parent_fee_state_of: non-genesis Fortuna parent parses its extra ──────
    #[test]
    fn parent_fee_state_acp176_parses_nonzero_parent() {
        let g = spec_from(0, 0, 0);
        let parent_state = Acp176State {
            gas: GasState {
                capacity: Gas(9_979_000),
                excess: Gas(21_000),
            },
            target_excess: Gas(0),
        };
        let parent = hdr(1, 200, Some(200_000), parent_state.to_bytes().to_vec());
        let state = parent_fee_state_of(&g, &parent).expect("parent fee state");
        assert_eq!(state, AvaFeeState::Acp176(parent_state));
    }

    // ── parent_fee_state_of: Window regime returns the raw window + base fee ───
    #[test]
    fn parent_fee_state_window_regime() {
        let w = spec_from(u64::MAX, u64::MAX, 0);
        let mut window = Window::default();
        window.add(&[5_000_000]);
        let parent = hdr(1, 200, None, window.to_bytes().to_vec());
        match parent_fee_state_of(&w, &parent).expect("parent fee state") {
            AvaFeeState::Window {
                window: got_window,
                base_fee,
            } => {
                assert_eq!(got_window, window, "raw parent window parsed");
                assert_eq!(base_fee, U256::from(25_000_000_000u64), "parent base fee");
            }
            other => panic!("expected Window regime, got {other:?}"),
        }
    }

    // ── min_delay_excess_of: pre-Granite None; Granite carries parent excess ───
    #[test]
    fn min_delay_excess_gating_and_carry() {
        // Pre-Granite child => None.
        let f = spec_from(0, u64::MAX, 0);
        let parent = hdr(0, 100, Some(100_000), vec![]);
        assert_eq!(
            min_delay_excess_of(&f, &parent, 200, None).expect("pre-granite"),
            None
        );

        // Granite child, Granite parent carrying an excess => carried (no desired).
        let g = spec_from(0, 0, 0);
        let mut granite_parent = hdr(1, 100, Some(100_000), vec![]);
        granite_parent.min_delay_excess = Some(INITIAL_DELAY_EXCESS.0);
        assert_eq!(
            min_delay_excess_of(&g, &granite_parent, 200, None).expect("granite"),
            Some(INITIAL_DELAY_EXCESS.0)
        );

        // Granite child, Granite parent MISSING its excess => error.
        let mut nil_parent = hdr(1, 100, Some(100_000), vec![]);
        nil_parent.min_delay_excess = None;
        let err = min_delay_excess_of(&g, &nil_parent, 200, None).expect_err("nil parent excess");
        assert!(matches!(err, Error::InvalidFeeState(_)), "got {err:?}");
    }

    // ── min_delay_excess_of: pre-Granite parent seeds InitialDelayExcess ──────
    #[test]
    fn min_delay_excess_seeds_initial_when_parent_pre_granite() {
        // Granite activates at 150: a child at 200 with a parent at 100 (pre-Granite)
        // starts from InitialDelayExcess and, with a desired move, steps toward it.
        let g = spec_from(0, 150, 0);
        let parent = hdr(1, 100, Some(100_000), vec![]); // parent pre-Granite
        // Desired one step up: InitialDelayExcess moved by MaxDelayExcessDiff (200).
        let desired = DelayExcess(INITIAL_DELAY_EXCESS.0 + 10_000);
        let got = min_delay_excess_of(&g, &parent, 200, Some(desired))
            .expect("granite child, pre-granite parent")
            .expect("some at granite");
        assert_eq!(got, INITIAL_DELAY_EXCESS.0 + 200, "moved by at most Q=200");
    }
}

#[cfg(test)]
mod semantic_verify_tests {
    use ava_evm_reth::{Address, B256, Bytes, Chain, U256, keccak256};

    use super::{
        CORTINA_GAS_LIMIT, MAX_FUTURE_BLOCK_TIME_MS, gas_capacity, verify_gas_used,
        verify_min_delay_excess, verify_time,
    };
    use crate::block::AvaHeader;
    use crate::chainspec::{AvaChainSpec, NetworkUpgrades};
    use crate::error::Error;
    use crate::feerules::acp226::INITIAL_DELAY_EXCESS;

    // Repeat of fee_state_tests::spec_from (test convention: repeat-don't-import).
    fn spec_from(fortuna: u64, granite: u64, ap3: u64) -> AvaChainSpec {
        const FF: u64 = u64::MAX;
        let upgrades = NetworkUpgrades {
            apricot_phase_1: 0,
            apricot_phase_2: 0,
            apricot_phase_3: ap3,
            apricot_phase_4: ap3,
            apricot_phase_5: ap3,
            apricot_phase_pre_6: ap3,
            apricot_phase_6: ap3,
            apricot_phase_post_6: ap3,
            banff: ap3,
            cortina: ap3,
            durango: ap3,
            etna: fortuna.min(granite),
            fortuna,
            granite,
            helicon: FF,
        };
        AvaChainSpec::from_parts(upgrades, Chain::from_id(43112), false)
    }

    // Repeat of fee_state_tests::hdr, plus a min_delay_excess parameter.
    fn hdr(number: u64, time: u64, time_ms: Option<u64>, mde: Option<u64>) -> AvaHeader {
        AvaHeader {
            parent_hash: B256::ZERO,
            uncle_hash: B256::ZERO,
            coinbase: Address::ZERO,
            state_root: B256::ZERO,
            tx_root: B256::ZERO,
            receipt_root: B256::ZERO,
            bloom: Bytes::from(vec![0u8; 256]),
            difficulty: U256::ZERO,
            number,
            gas_limit: 15_000_000,
            gas_used: 0,
            time,
            extra: Bytes::new(),
            mix_digest: B256::ZERO,
            nonce: [0u8; 8],
            ext_data_hash: keccak256([]),
            base_fee: Some(U256::from(25_000_000_000u64)),
            ext_data_gas_used: None,
            block_gas_cost: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_root: None,
            time_milliseconds: time_ms,
            min_delay_excess: mde,
        }
    }

    const T: u64 = 1_700_000_000; // an arbitrary base timestamp (seconds)

    #[test]
    fn verify_time_pre_granite_equal_timestamp_ok() {
        // time.go:65-70 — equality allowed (multiple blocks per second pre-Granite).
        let spec = spec_from(0, u64::MAX, 0); // Granite never active
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T, None, None);
        assert!(
            verify_time(&spec, &parent, &header, T.checked_mul(1000).unwrap()).is_ok(),
            "verify_time(equal pre-Granite timestamps)"
        );
    }

    #[test]
    fn verify_time_rejects_block_older_than_parent() {
        // time.go:68-70 — errBlockTooOld.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T - 1, None, None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, T * 1000),
            Err(Error::BlockTooOld { .. })
        ));
    }

    #[test]
    fn verify_time_future_bound_is_inclusive() {
        // time.go:72-79 — exactly now+10s is allowed; one ms over rejects.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T + 10, None, None); // header_ms = (T+10)*1000
        let now_ms = T * 1000; // max allowed = now_ms + 10_000 == header_ms
        assert!(verify_time(&spec, &parent, &header, now_ms).is_ok());
        assert!(matches!(
            verify_time(&spec, &parent, &header, now_ms - 1),
            Err(Error::BlockTooFarInFuture { .. })
        ));
        // Sanity on the constant itself (time.go:20).
        assert_eq!(MAX_FUTURE_BLOCK_TIME_MS, 10_000);
    }

    #[test]
    fn verify_time_rejects_time_milliseconds_before_granite() {
        // time.go:81-86 — ErrTimeMillisecondsBeforeGranite.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T, Some(T * 1000), None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, T * 1000),
            Err(Error::TimeMillisecondsBeforeGranite)
        ));
    }

    #[test]
    fn verify_time_requires_time_milliseconds_at_granite() {
        // time.go:89-92 — ErrTimeMillisecondsRequired.
        let spec = spec_from(0, 0, 0); // Granite from genesis
        let parent = hdr(1, T, Some(T * 1000), None);
        let header = hdr(2, T + 2, None, None);
        assert!(matches!(
            verify_time(&spec, &parent, &header, (T + 2) * 1000),
            Err(Error::TimeMillisecondsRequired)
        ));
    }

    #[test]
    fn verify_time_rejects_mismatched_time_milliseconds() {
        // time.go:94-101 — ErrTimeMillisecondsMismatched.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), None);
        let header = hdr(2, T + 2, Some((T + 3) * 1000), None); // 	ime != ms/1000
        assert!(matches!(
            verify_time(&spec, &parent, &header, (T + 3) * 1000),
            Err(Error::TimeMillisecondsMismatched { .. })
        ));
    }

    #[test]
    fn verify_time_first_granite_block_skips_min_delay() {
        // time.go:103-108 — parent without MinDelayExcess is exempt.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), None); // no excess
        let header = hdr(2, T, Some(T * 1000 + 1), None); // 1ms delay
        assert!(verify_time(&spec, &parent, &header, T * 1000 + 1).is_ok());
    }

    #[test]
    fn verify_time_enforces_min_delay_boundary() {
        // time.go:110-121 — actual delay < required rejects; == passes. Each
        // header derives `time` as `ms / 1000` so the Mismatched arm cannot
        // fire first, whatever value `required` happens to be.
        let spec = spec_from(0, 0, 0);
        let required = INITIAL_DELAY_EXCESS.delay();
        assert!(required > 0, "test premise: initial excess demands a delay");
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let exact_ms = T * 1000 + required;
        let exact = hdr(2, exact_ms / 1000, Some(exact_ms), None);
        let short = hdr(2, (exact_ms - 1) / 1000, Some(exact_ms - 1), None);
        let now = exact_ms;
        assert!(verify_time(&spec, &parent, &exact, now).is_ok());
        assert!(matches!(
            verify_time(&spec, &parent, &short, now),
            Err(Error::MinDelayNotMet { .. })
        ));
    }

    #[test]
    fn verify_min_delay_excess_pre_granite_is_noop() {
        // min_delay_excess.go:50-52.
        let spec = spec_from(0, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        let header = hdr(2, T + 2, None, None);
        assert!(verify_min_delay_excess(&spec, &parent, &header).is_ok());
    }

    #[test]
    fn verify_min_delay_excess_requires_field_at_granite() {
        // min_delay_excess.go:54-57 — errRemoteMinDelayExcessNil.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), None);
        assert!(matches!(
            verify_min_delay_excess(&spec, &parent, &header),
            Err(Error::RemoteMinDelayExcessNil)
        ));
    }

    #[test]
    fn verify_min_delay_excess_accepts_reachable_claim() {
        // min_delay_excess.go:59-71 — claimed-as-desired: an unchanged claim
        // is always reachable (update toward itself is a no-op).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), Some(INITIAL_DELAY_EXCESS.0));
        assert!(verify_min_delay_excess(&spec, &parent, &header).is_ok());
    }

    #[test]
    fn verify_min_delay_excess_rejects_unreachable_claim() {
        // min_delay_excess.go:73-79 — errIncorrectMinDelayExcess: a claim the
        // one-step update from the parent cannot reach recomputes lower.
        let spec = spec_from(0, 0, 0);
        let parent = hdr(1, T, Some(T * 1000), Some(INITIAL_DELAY_EXCESS.0));
        let header = hdr(2, T + 2, Some((T + 2) * 1000), Some(u64::MAX));
        assert!(matches!(
            verify_min_delay_excess(&spec, &parent, &header),
            Err(Error::IncorrectMinDelayExcess { .. })
        ));
    }

    #[test]
    fn gas_capacity_pre_fortuna_static_limits() {
        // gas_limit.go:170-173 → GasLimit: Cortina 15M / AP1 8M / pre-AP1
        // parent.gas_limit (gas_limit.go:52-57).
        let cortina = spec_from(u64::MAX, u64::MAX, 0);
        let parent = hdr(1, T, None, None);
        assert_eq!(
            gas_capacity(&cortina, &parent, T * 1000).unwrap(),
            CORTINA_GAS_LIMIT
        );
    }

    #[test]
    fn gas_capacity_fortuna_uses_pre_block_fee_state() {
        // gas_limit.go:175-179 — the ACP-176 capacity. A genesis parent whose
        // elapsed time saturates the fill gives capacity == MaxCapacity (10M
        // at the default target excess — the live-block-1 golden numbers in
        // fee_state_tests::after_block_matches_live_block1_numbers).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(0, T, Some(T * 1000), None);
        let cap = gas_capacity(&spec, &parent, (T + 1_000_000) * 1000).unwrap();
        assert_eq!(
            cap, 10_000_000,
            "saturated ACP-176 capacity at default target"
        );
    }

    #[test]
    fn verify_gas_used_boundary() {
        // gas_limit.go:90-96 — errInvalidGasUsed: > capacity rejects, == passes.
        let spec = spec_from(u64::MAX, u64::MAX, 0); // Cortina static 15M
        let parent = hdr(1, T, None, None);
        let mut ok_hdr = hdr(2, T + 2, None, None);
        ok_hdr.gas_used = CORTINA_GAS_LIMIT;
        assert!(verify_gas_used(&spec, &parent, &ok_hdr).is_ok());
        let mut bad = hdr(2, T + 2, None, None);
        bad.gas_used = CORTINA_GAS_LIMIT + 1;
        assert!(matches!(
            verify_gas_used(&spec, &parent, &bad),
            Err(Error::GasUsedOverCapacity { .. })
        ));
    }

    #[test]
    fn verify_gas_used_folds_ext_data_gas_at_fortuna() {
        // gas_limit.go:69-82 — Fortuna+: gasUsed + extDataGasUsed vs capacity;
        // a non-u64 claim errors (errInvalidExtraDataGasUsed).
        let spec = spec_from(0, 0, 0);
        let parent = hdr(0, T, Some(T * 1000), None);
        let ms = (T + 1_000_000) * 1000;
        let mut h = hdr(2, T + 1_000_000, Some(ms), None);
        h.gas_used = 9_999_999;
        h.ext_data_gas_used = Some(U256::from(2u64)); // 10_000_001 > 10M
        assert!(matches!(
            verify_gas_used(&spec, &parent, &h),
            Err(Error::GasUsedOverCapacity { .. })
        ));
        h.ext_data_gas_used = Some(U256::from(u128::from(u64::MAX)) + U256::from(1u64));
        assert!(matches!(
            verify_gas_used(&spec, &parent, &h),
            Err(Error::ExtDataGasUsedTooLarge(_))
        ));
    }
}
