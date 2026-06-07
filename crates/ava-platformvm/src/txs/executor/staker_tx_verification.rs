// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staker-tx verification for the tx executors
//! (`vms/platformvm/txs/executor/staker_tx_verification.go`, specs 08 §2.4).
//!
//! These helpers carry out the semantic validation of the permissionless
//! staking txs the standard executor accepts: weight & duration bounds, the
//! BLS-key / staked-asset rules, start-time bounds, subnet-validator overlap,
//! the primary-network-membership requirement for subnet validators, and the
//! single-asset flow check. They are `pub(crate)` so the sibling executors can
//! reuse the shared start-time / validator-lookup helpers.

use std::time::{Duration, SystemTime};

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::staker::Staker;
use crate::txs::AddPermissionlessDelegatorTx;
use crate::txs::AddPermissionlessValidatorTx;

use super::backend::Backend;
use super::state_changes;

/// `SyncBound` — the permitted clock-skew slack when checking a pre-Durango
/// staker start time (`executor.SyncBound = 10s`).
pub(crate) const SYNC_BOUND: Duration = Duration::from_secs(10);

/// `MaxFutureStartTime` — pre-Durango, a staker's start time may be at most this
/// far in the future (`executor.MaxFutureStartTime = 24 * 7 * 2 hours`).
pub(crate) const MAX_FUTURE_START_TIME: Duration = Duration::from_secs(24 * 7 * 2 * 60 * 60);

/// `GetValidator` — the current-or-pending validator of `subnet` run by `node`,
/// or [`Error::Database`] (`database.ErrNotFound`) if neither set has it.
///
/// `Chain` exposes a point lookup only for current validators; the pending set
/// is scanned (the diff overlay keeps it small).
///
/// # Errors
/// Returns [`Error::Database`] if `node` validates neither the current nor the
/// pending set of `subnet`.
pub(crate) fn get_validator(chain: &dyn Chain, subnet: Id, node: NodeId) -> Result<Staker> {
    if let Ok(s) = chain.get_current_validator(subnet, node) {
        return Ok(s);
    }
    chain
        .pending_stakers()
        .into_iter()
        .find(|s| s.subnet_id == subnet && s.node_id == node && s.priority.is_validator())
        .ok_or(Error::Database(ava_database::error::Error::NotFound))
}

/// `verifyStakerStartTime` — pre-Durango, the start time must be strictly after
/// the chain time and within [`MAX_FUTURE_START_TIME`] (with [`SYNC_BOUND`]
/// slack). Post-Durango the start time is not validated.
///
/// # Errors
/// - [`Error::TimestampNotBeforeStartTime`] if `staker_time <= chain_time`.
/// - [`Error::TimeTooAdvanced`] if `staker_time` is too far in the future.
pub(crate) fn verify_staker_start_time(
    is_durango_active: bool,
    chain_time: SystemTime,
    staker_time: SystemTime,
) -> Result<()> {
    if is_durango_active {
        return Ok(());
    }
    // Start time must be after the current chain time.
    if staker_time <= chain_time {
        return Err(Error::TimestampNotBeforeStartTime);
    }
    // Start time must be at most MaxFutureStartTime in the future (+ SyncBound).
    let limit = chain_time
        .checked_add(MAX_FUTURE_START_TIME)
        .and_then(|t| t.checked_add(SYNC_BOUND))
        .unwrap_or(staker_time);
    if staker_time > limit {
        return Err(Error::TimeTooAdvanced);
    }
    Ok(())
}

/// `txs.BoundedBy` — `[small_start, small_end]` is inside `[large_start,
/// large_end]`.
fn bounded_by(
    small_start: SystemTime,
    small_end: SystemTime,
    large_start: SystemTime,
    large_end: SystemTime,
) -> bool {
    large_start <= small_start && small_end <= large_end
}

/// Converts a `u64` unix timestamp (the tx's `start`/`end` field) to a
/// [`SystemTime`].
fn unix(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `verifyAddPermissionlessValidatorTx` — semantic validation for an
/// [`AddPermissionlessValidatorTx`] (specs 08 §2.4).
///
/// Checks (post-bootstrap): start-time bounds, weight/duration/delegation-fee/
/// staked-asset bounds, no duplicate validator on the subnet, the
/// primary-network-membership requirement for subnet validators, and the
/// single-asset flow check (fee charged via the fork-selected calculator). The
/// BLS-key-present-iff-primary rule is enforced by the tx's `syntactic_verify`.
///
/// # Errors
/// Returns the matching [`Error`] variant on any failed check.
pub(crate) fn verify_add_permissionless_validator(
    backend: &Backend,
    chain: &dyn Chain,
    unsigned_bytes: &[u8],
    tx: &AddPermissionlessValidatorTx,
) -> Result<()> {
    let _ = unsigned_bytes;
    tx.syntactic_verify()?;

    let current_timestamp = chain.timestamp();
    let is_durango_active = backend.is_durango_activated(current_timestamp);

    if !backend.bootstrapped {
        return Ok(());
    }

    let start_time = if is_durango_active {
        current_timestamp
    } else {
        unix(tx.validator.start)
    };
    let end_time = unix(tx.validator.end);
    let duration = end_time
        .duration_since(start_time)
        .map_err(|_| Error::StakeTooShort)?;

    verify_staker_start_time(is_durango_active, current_timestamp, start_time)?;

    let is_primary = tx.subnet == Id::EMPTY;
    let staked_asset = tx.stake_outs.first().ok_or(Error::NoStake)?.asset_id();
    let cfg = &backend.staking;

    if tx.validator.wght < cfg.min_validator_stake {
        return Err(Error::WeightTooSmall);
    }
    if tx.validator.wght > cfg.max_validator_stake {
        return Err(Error::WeightTooLarge);
    }
    if u64::from(tx.delegation_shares) < u64::from(cfg.min_delegation_fee) {
        return Err(Error::InsufficientDelegationFee);
    }
    if duration < cfg.min_stake_duration {
        return Err(Error::StakeTooShort);
    }
    if duration > cfg.max_stake_duration {
        return Err(Error::StakeTooLong);
    }
    // The Primary Network stakes AVAX; subnets define their own asset via a
    // subnet transformation (not yet ported), so subnet staking-asset checks are
    // limited to the primary-network rule here.
    if is_primary && staked_asset != backend.avax_asset_id {
        return Err(Error::WrongStakedAssetId);
    }

    // No duplicate validator on the subnet.
    if get_validator(chain, tx.subnet, tx.validator.node_id).is_ok() {
        return Err(Error::DuplicateValidator);
    }

    // Subnet validators must be inside their primary-network validation window.
    if !is_primary {
        verify_subnet_validator_primary_network_requirements(
            is_durango_active,
            chain,
            tx.validator.node_id,
            start_time,
            end_time,
        )?;
    }

    // Flow check: fee charged on AVAX, conserved against ins / (outs + stake).
    // The staked outputs are produced (locked) alongside the base change, so
    // they count toward `producedAVAX` (Go `utxo.GetInputOutputs`).
    let mut outs = tx.base.base.outs.clone();
    outs.extend(tx.stake_outs.iter().cloned());
    let fee = state_changes::fee_calculator(backend, chain)
        .calculate_fee(crate::txs::fee::complexity::base_tx_complexity())?;
    state_changes::verify_spend(chain, &tx.base.base.ins, &outs, fee, backend.avax_asset_id)
}

/// `verifyAddPermissionlessDelegatorTx` — semantic validation for an
/// [`AddPermissionlessDelegatorTx`] (specs 08 §2.4).
///
/// Checks (post-bootstrap): start-time bounds, weight/duration bounds, the
/// referenced validator exists and the delegation window is inside the
/// validator's window, and the single-asset flow check.
///
/// # Errors
/// Returns the matching [`Error`] variant on any failed check.
pub(crate) fn verify_add_permissionless_delegator(
    backend: &Backend,
    chain: &dyn Chain,
    unsigned_bytes: &[u8],
    tx: &AddPermissionlessDelegatorTx,
) -> Result<()> {
    let _ = unsigned_bytes;
    // `AddPermissionlessDelegatorTx` has no `syntactic_verify` helper (M4.3/4.4
    // did not generate one); reproduce the relevant checks inline.
    tx.base.syntactic_verify()?;
    tx.validator.verify()?;
    if tx.stake_outs.is_empty() {
        return Err(Error::NoStake);
    }

    let current_timestamp = chain.timestamp();
    let is_durango_active = backend.is_durango_activated(current_timestamp);

    if !backend.bootstrapped {
        return Ok(());
    }

    let start_time = if is_durango_active {
        current_timestamp
    } else {
        unix(tx.validator.start)
    };
    let end_time = unix(tx.validator.end);
    let duration = end_time
        .duration_since(start_time)
        .map_err(|_| Error::StakeTooShort)?;

    verify_staker_start_time(is_durango_active, current_timestamp, start_time)?;

    let cfg = &backend.staking;
    if tx.validator.wght < cfg.min_delegator_stake {
        return Err(Error::WeightTooSmall);
    }
    if duration < cfg.min_stake_duration {
        return Err(Error::StakeTooShort);
    }
    if duration > cfg.max_stake_duration {
        return Err(Error::StakeTooLong);
    }

    // The delegated-to validator must exist and the delegation window must be a
    // subset of the validator's window.
    let validator =
        get_validator(chain, tx.subnet, tx.validator.node_id).map_err(|_| Error::NotValidator)?;
    if !bounded_by(
        start_time,
        end_time,
        validator.start_time,
        validator.end_time,
    ) {
        return Err(Error::PeriodMismatch);
    }

    let mut outs = tx.base.base.outs.clone();
    outs.extend(tx.stake_outs.iter().cloned());
    let fee = state_changes::fee_calculator(backend, chain)
        .calculate_fee(crate::txs::fee::complexity::base_tx_complexity())?;
    state_changes::verify_spend(chain, &tx.base.base.ins, &outs, fee, backend.avax_asset_id)
}

/// `verifySubnetValidatorPrimaryNetworkRequirements` — a subnet validator's
/// `node` must be a primary-network validator whose window contains the subnet
/// validation window (specs 08 §2.4).
///
/// # Errors
/// - [`Error::NotValidator`] if `node` is not a primary-network validator.
/// - [`Error::PeriodMismatch`] if the subnet window is not inside the primary
///   window.
pub(crate) fn verify_subnet_validator_primary_network_requirements(
    is_durango_active: bool,
    chain: &dyn Chain,
    node: NodeId,
    start_time: SystemTime,
    end_time: SystemTime,
) -> Result<()> {
    let primary = get_validator(chain, Id::EMPTY, node).map_err(|_| Error::NotValidator)?;

    // Pre-Durango the comparison uses the staker's start; post-Durango it uses
    // the chain time (already folded into `start_time` by the caller).
    let _ = is_durango_active;
    if !bounded_by(start_time, end_time, primary.start_time, primary.end_time) {
        return Err(Error::PeriodMismatch);
    }
    Ok(())
}
