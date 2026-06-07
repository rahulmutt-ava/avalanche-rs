// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Shared state-mutation / flow-check helpers for the tx executors
//! (`vms/platformvm/txs/executor/state_changes.go`, specs 08 §2.4).
//!
//! Go's `state_changes.go` houses the advance-time helpers (`AdvanceTimeTo`,
//! the staker promotion/removal pass) used by the proposal executor (M4.17), and
//! the per-tx flow-check glue used by every executor. M4.16 ports the pieces the
//! standard executor needs: the **fork-selected fee calculator** and the
//! **single-asset flow check** that composes with the M4.15 UTXO handler. The
//! time-advancement pass itself is M4.17's; this module exposes only the shared
//! surface so the siblings can build on it additively.

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::txs::components::{TransferableInput, TransferableOutput};
use crate::txs::fee::FeeCalculator;
use crate::utxo;

use super::backend::Backend;

/// Selects the fee regime in force at the chain timestamp (`fee.Calculator`,
/// specs 08 §6): the ACP-103 dynamic gas fee once Etna is active, else the flat
/// static fee.
///
/// `chain` supplies the current gas excess (for the dynamic price) and the chain
/// timestamp (for the fork selection).
#[must_use]
pub(crate) fn fee_calculator(backend: &Backend, chain: &dyn Chain) -> FeeCalculator {
    let etna_active = backend.is_etna_activated(chain.timestamp());
    FeeCalculator::for_fork(
        etna_active,
        backend.static_fee_config.tx_fee,
        chain.fee_state().excess,
    )
}

/// `backend.FlowChecker.VerifySpend` (single-asset slice) — verifies that the
/// AVAX consumed by `ins` equals the AVAX produced by `outs` plus `fee`, with
/// every consumed UTXO present in `chain` (specs 08 §2.4 `VerifySpendUTXOs`).
///
/// This composes with the M4.15 UTXO handler's [`utxo::verify_spend`] (the
/// byte-stored UTXO model). The full multi-asset / locktime-aware credential
/// check is layered on as the credential-verifying flow checker matures; the
/// single-asset AVAX conservation is what the decision txs ported here exercise.
///
/// # Errors
/// Returns [`Error::FlowCheckFailed`] (wrapping the underlying conservation /
/// lookup failure) if the spend does not balance.
pub(crate) fn verify_spend(
    chain: &dyn Chain,
    ins: &[TransferableInput],
    outs: &[TransferableOutput],
    fee: u64,
    avax_asset_id: ava_types::id::Id,
) -> Result<()> {
    utxo::verify_spend(chain, ins, outs, fee, avax_asset_id).map_err(|_| Error::FlowCheckFailed)
}
