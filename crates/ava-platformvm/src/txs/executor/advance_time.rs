// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The time-advancement state pass (`vms/platformvm/txs/executor/state_changes.go`
//! `AdvanceTimeTo`, specs 08 §2.4 / §3.3 / 21 §2b).
//!
//! [`advance_time_to`] advances a [`Diff`]'s chain time to `new_time`, applying
//! the staker-set transitions and (post-Etna) the dynamic-fee + ACP-77
//! continuous-fee accounting that the proposal executor (and the block executor)
//! depend on:
//!
//! 1. **Promote** every pending staker whose `start_time <= new_time` into the
//!    current set, minting its potential reward into the subnet supply. Pending
//!    *validators* are promoted before pending *delegators* (Go's two-pass order)
//!    so the defensive "a current delegator must have a current validator" check
//!    is satisfied even when a tie orders a pending delegator first.
//! 2. **Remove** every current *permissioned subnet* validator whose
//!    `end_time <= new_time` (priority [`SubnetPermissionedValidatorCurrent`]).
//!    Permissionless stakers are *not* removed here — they leave via a
//!    [`RewardValidatorTx`](crate::txs::RewardValidatorTx) after time advances.
//! 3. Post-Etna only: advance the dynamic gas-fee excess and charge the L1
//!    continuous fee, deactivating exhausted L1 validators in
//!    `EndAccumulatedFee` order (specs 21 §2b).
//!
//! As in Go, the staker iterators are read from the *parent* view (the `Diff`'s
//! fall-through reads) while the mutations are recorded in the same `Diff`'s
//! overlay; the overlay is not consulted mid-iteration, so the reads stay stable.
//!
//! [`SubnetPermissionedValidatorCurrent`]: crate::txs::Priority::SubnetPermissionedValidatorCurrent

use std::time::SystemTime;

use crate::error::{Error, Result};
use crate::reward::Calculator;
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::txs::Priority;
use crate::validators::fee::{L1Config, L1State};

use super::backend::Backend;

/// Maps a pending priority to its current-set counterpart
/// (`txs.PendingToCurrentPriorities`). A current priority maps to itself.
const fn pending_to_current(priority: Priority) -> Priority {
    match priority {
        Priority::PrimaryNetworkValidatorPending => Priority::PrimaryNetworkValidatorCurrent,
        Priority::PrimaryNetworkDelegatorApricotPending
        | Priority::PrimaryNetworkDelegatorBanffPending => Priority::PrimaryNetworkDelegatorCurrent,
        Priority::SubnetPermissionlessValidatorPending => {
            Priority::SubnetPermissionlessValidatorCurrent
        }
        Priority::SubnetPermissionlessDelegatorPending => {
            Priority::SubnetPermissionlessDelegatorCurrent
        }
        Priority::SubnetPermissionedValidatorPending => {
            Priority::SubnetPermissionedValidatorCurrent
        }
        // Already-current priorities are unchanged.
        other => other,
    }
}

/// `AdvanceTimeTo` — advance `diff`'s chain time to `new_time`, applying the
/// staker promotion/removal pass and (post-Etna) the fee-state accounting.
///
/// Returns `true` if the validator set changed (a staker was promoted/removed,
/// or an L1 validator was deactivated), mirroring Go's `changed` flag.
///
/// # Errors
/// - [`Error::Database`] if a subnet supply cannot be read.
/// - [`Error::Overflow`] if the supply mint or the accrued-fee sum overflows
///   `u64`, or a duration exceeds `u64` nanoseconds.
pub(crate) fn advance_time_to(
    backend: &Backend,
    diff: &mut Diff,
    new_time: SystemTime,
) -> Result<bool> {
    let mut changed = false;

    // --- 1. Promote pending stakers whose start_time <= new_time. ---
    //
    // Buffer the promotions so validators are applied before delegators (Go's
    // two-pass order), regardless of the iterator tie-break order.
    let mut validator_promotions = Vec::new();
    let mut delegator_promotions = Vec::new();

    for pending in diff.pending_stakers() {
        if pending.start_time > new_time {
            // The pending iterator is ordered by next_time (== start_time while
            // pending), so the first future start ends the pass.
            break;
        }

        let mut to_add = pending.clone();
        to_add.next_time = pending.end_time;
        to_add.priority = pending_to_current(pending.priority);

        // Only permissionless networks (incl. the primary network) earn a
        // potential reward; permissioned subnet validators do not.
        if pending.priority != Priority::SubnetPermissionedValidatorPending {
            let supply = diff.current_supply(pending.subnet_id)?;
            let calc = Calculator::new(backend.staking.reward_config);
            let stake_duration = pending
                .end_time
                .duration_since(pending.start_time)
                .map_err(|_| Error::StakeTooShort)?;
            let stake_duration_ns =
                u64::try_from(stake_duration.as_nanos()).map_err(|_| Error::Overflow)?;
            let potential_reward = calc.calculate(stake_duration_ns, pending.weight, supply);
            to_add.potential_reward = potential_reward;

            // Invariant: the calculator never returns a reward that overflows the
            // supply cap, so the add is in-range; guard anyway.
            let new_supply = supply
                .checked_add(potential_reward)
                .ok_or(Error::Overflow)?;
            diff.set_current_supply(pending.subnet_id, new_supply);
        }

        if pending.priority.is_pending_validator() {
            validator_promotions.push((pending, to_add));
        } else if pending.priority.is_pending_delegator() {
            delegator_promotions.push((pending, to_add));
        } else {
            // A non-pending priority in the pending set is malformed.
            return Err(Error::WrongTxType);
        }
    }

    for (pending, current) in &validator_promotions {
        diff.put_current_validator(current.clone())?;
        diff.delete_pending_validator(pending);
    }
    for (pending, current) in &delegator_promotions {
        diff.put_current_delegator(current.clone());
        diff.delete_pending_delegator(pending);
    }
    changed = changed || !validator_promotions.is_empty() || !delegator_promotions.is_empty();

    // --- 2. Remove current permissioned subnet validators whose end <= new_time. ---
    for staker in diff.current_stakers() {
        if staker.end_time > new_time {
            break;
        }
        // Permissioned stakers have the smallest current priority, so they are
        // encountered first for a given end time; a permissionless staker ends
        // this pass (it is removed by a RewardValidatorTx instead).
        if staker.priority != Priority::SubnetPermissionedValidatorCurrent {
            break;
        }
        diff.delete_current_validator(&staker);
        changed = true;
    }

    // Pre-Etna: only the timestamp advances; the dynamic-fee and L1 continuous-fee
    // accounting is Etna-gated.
    if !backend.is_etna_activated(new_time) {
        diff.set_timestamp(new_time);
        return Ok(changed);
    }

    // --- 3. Post-Etna fee-state accounting. ---
    let previous_time = diff.timestamp();
    let seconds = new_time
        .duration_since(previous_time)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let l1_changed = advance_validator_fee_state(backend, diff, seconds)?;
    changed = changed || l1_changed;

    diff.set_timestamp(new_time);
    Ok(changed)
}

/// `advanceValidatorFeeState` — charge the ACP-77 continuous fee over `seconds`,
/// deactivating L1 validators whose `EndAccumulatedFee` is now reached
/// (specs 21 §2b). The active L1 validators are read from the (parent) view and
/// the deactivations are written into the same `diff`.
///
/// Returns `true` if any L1 validator was deactivated.
fn advance_validator_fee_state(backend: &Backend, diff: &mut Diff, seconds: u64) -> Result<bool> {
    let actives = diff.active_l1_validators();
    let current = u64::try_from(actives.len()).map_err(|_| Error::Overflow)?;
    let fee_state = L1State {
        current,
        excess: diff.l1_validator_excess(),
    };
    let config = l1_config(backend);

    let validator_cost = fee_state.cost_of(&config, seconds);
    let accrued_fees = diff
        .accrued_fees()
        .checked_add(validator_cost)
        .ok_or(Error::Overflow)?;

    // Deactivate every active L1 validator whose EndAccumulatedFee is now
    // covered by the accrued fees; the iterator is in increasing
    // EndAccumulatedFee order, so the first un-covered one ends the pass.
    let mut changed = false;
    for mut v in actives {
        if v.end_accumulated_fee > accrued_fees {
            break;
        }
        v.end_accumulated_fee = 0; // Deactivate.
        diff.put_l1_validator(v)?;
        changed = true;
    }

    let advanced = fee_state.advance_time(config.target, seconds);
    diff.set_l1_validator_excess(advanced.excess);
    diff.set_accrued_fees(accrued_fees);
    Ok(changed)
}

/// The L1 continuous-fee [`L1Config`] for `backend`'s network (mainnet vs the
/// other networks differ only in the excess-conversion constant `K`).
fn l1_config(backend: &Backend) -> L1Config {
    let k = if backend.network_id == 1 {
        crate::validators::fee::K_MAINNET
    } else {
        crate::validators::fee::K_FUJI
    };
    L1Config {
        target: crate::validators::fee::TARGET,
        min_price: crate::validators::fee::MIN_PRICE,
        k,
    }
}
