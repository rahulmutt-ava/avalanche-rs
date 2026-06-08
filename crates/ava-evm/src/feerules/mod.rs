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

use ava_evm_reth::Header;

use crate::atomic::tx::{COST_PER_SIGNATURE, EVM_INPUT_GAS, EVM_OUTPUT_GAS, TX_BYTES_GAS};
use crate::chainspec::{AvaChainSpec, AvaPhase};
use crate::error::Error;
use crate::evmconfig::{AvaFeeState, AvaNextBlockCtx};

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
/// - **Fortuna+** ([`FeeRegime::Acp176`]) → the ACP-176 gas price of the
///   carried [`AvaFeeState::Acp176`] state.
///
/// The window arm returns a `u64` (C-Chain base fees fit in `u64`); a value that
/// exceeds `u64::MAX` saturates (it would already be clamped to a phase bound
/// well below that).
///
/// # Errors
/// Returns [`Error::NilBaseFee`] pre-AP3, or if the carried fee-state does not
/// match the active regime (a programming error in the builder wiring).
pub fn base_fee(cs: &AvaChainSpec, parent: &Header, ctx: &AvaNextBlockCtx) -> Result<u64, Error> {
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
            let time_elapsed = ctx.timestamp.saturating_sub(parent.timestamp);
            let bf = window::base_fee_from_window(params, &window, parent_base, time_elapsed);
            Ok(u64::try_from(bf).unwrap_or(u64::MAX))
        }
        FeeRegime::Acp176 => match &ctx.parent_fee_state {
            AvaFeeState::Acp176(state) => Ok(state.gas_price().0),
            // Builder wiring error: ACP-176 regime requires an acp176 state.
            AvaFeeState::Window { .. } => Err(Error::NilBaseFee),
        },
    }
}

/// `gas_limit` — the per-fork block gas-limit dispatch (spec 10 §7.2/§17.3).
///
/// Pre-Cortina coreth uses the static `ApricotPhase1GasLimit`; Cortina raised it
/// to `CortinaGasLimit`. The builder may override via `ctx.gas_limit_hint`
/// (clamped to the active ceiling). For the ACP-176 regime the gas limit is the
/// dynamic max-capacity, but coreth keeps a fixed header `GasLimit`; we honour
/// the hint when present, else the phase default.
///
/// # Errors
/// Currently infallible, but returns `Result` to match the spec §17.3 signature
/// and leave room for the ACP-176 max-capacity gate.
pub fn gas_limit(cs: &AvaChainSpec, _parent: &Header, ctx: &AvaNextBlockCtx) -> Result<u64, Error> {
    let phase = cs.fork_at(ctx.timestamp);
    // coreth `params/avalanche_params.go`: ApricotPhase1GasLimit = 8_000_000,
    // CortinaGasLimit = 15_000_000.
    const APRICOT_PHASE1_GAS_LIMIT: u64 = 8_000_000;
    const CORTINA_GAS_LIMIT: u64 = 15_000_000;
    let default_limit = if phase >= AvaPhase::Cortina {
        CORTINA_GAS_LIMIT
    } else {
        APRICOT_PHASE1_GAS_LIMIT
    };
    Ok(ctx.gas_limit_hint.unwrap_or(default_limit))
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
