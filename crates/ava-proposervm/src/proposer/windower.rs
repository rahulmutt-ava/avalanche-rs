// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The windower — proposer scheduling (Go `vms/proposervm/proposer/windower.go`).
//!
//! Two regimes:
//!
//! - **Pre-Durango** ([`Windower::proposers`] / [`Windower::delay`]): a 32-bit
//!   `MT19937` seeded `chain_source ^ block_height` drives a weighted-without-
//!   replacement sample of `min(max_windows, total_weight)` indices; the
//!   validator at sample position `i` may build after `i * WINDOW_DURATION`.
//! - **Post-Durango** ([`Windower::expected_proposer`] /
//!   [`Windower::min_delay_for_proposer`]): a 64-bit `MT19937_64` seeded
//!   per-slot with `chain_source ^ block_height ^ reverse_bits64(slot)` draws a
//!   single index — the validator scheduled for that slot.
//!
//! Both share [`Windower::make_sampler`]: fetch the set at `p_chain_height`,
//! drop the empty `NodeId` (inactive ACP-77 validators), sort by `NodeId`, and
//! feed the weights to the deterministic sampler.
//!
//! **R1 (gonum MT compatibility):** the MT streams are the vendored
//! `ava_utils::rng::{Mt19937, Mt19937_64}` and the sampler is
//! `ava_utils::sampler::WeightedWithoutReplacementGeneric`. The MT is re-seeded
//! per sample by reconstructing a freshly-seeded sampler — bit-identical to Go's
//! `source.Seed(...)` followed by `sampler.Sample(...)` (the weighted-heap
//! `Initialize` is RNG-free and `Sample` resets the uniform), so the captured Go
//! `NodeId` orderings reproduce exactly (`golden::windower_schedule`).

use std::time::Duration;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::rng::{Mt19937, Mt19937_64};
use ava_utils::sampler::weighted_without_replacement::{
    WeightedWithoutReplacement, WeightedWithoutReplacementGeneric,
};
use ava_validators::state::ValidatorState;

use crate::error::{Error, Result};

/// The proposer window duration (Go `WindowDuration`).
pub const WINDOW_DURATION: Duration = Duration::from_secs(5);

/// Maximum number of verify windows (Go `MaxVerifyWindows`).
pub const MAX_VERIFY_WINDOWS: u64 = 6;

/// Maximum number of build windows (Go `MaxBuildWindows`).
pub const MAX_BUILD_WINDOWS: u64 = 60;

/// Maximum number of look-ahead slots scanned by `min_delay_for_proposer`
/// (Go `MaxLookAheadSlots`).
pub const MAX_LOOK_AHEAD_SLOTS: u64 = 720;

/// A single validator entry used by the windower's sampler (Go `validatorData`).
///
/// The list of these is always **sorted by `NodeId`** (sorting by weight would
/// not produce a canonical ordering) and never contains the empty `NodeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorData {
    /// The validator's node id.
    pub id: NodeId,
    /// The validator's weight.
    pub weight: u64,
}

/// The windower — computes proposer schedules for a given chain/subnet.
///
/// Owns a [`ValidatorState`] to resolve the validator set at a P-Chain height,
/// plus the precomputed `chain_source` (the big-endian `u64` of the first 8
/// bytes of the chain id).
pub struct Windower<S: ValidatorState> {
    state: S,
    subnet_id: Id,
    chain_source: u64,
}

impl<S: ValidatorState> Windower<S> {
    /// Builds a windower for `chain_id` within `subnet_id` (Go `New`).
    #[must_use]
    pub fn new(state: S, subnet_id: Id, chain_id: Id) -> Self {
        Self {
            state,
            subnet_id,
            chain_source: chain_source(chain_id),
        }
    }

    /// The `chain_source` seed component (BE `u64` of the chain id's first 8
    /// bytes).
    #[must_use]
    pub fn chain_source(&self) -> u64 {
        self.chain_source
    }

    /// Borrows the underlying [`ValidatorState`] (used by the VM wrapper's
    /// `selectChildPChainHeight` to read the recommended minimum height).
    #[must_use]
    pub fn validator_state(&self) -> &S {
        &self.state
    }

    /// Fetches and canonicalizes the validator set at `p_chain_height`:
    /// the empty `NodeId` is dropped and the rest are sorted by `NodeId`
    /// (Go `makeSampler`, minus the sampler — the sampler is constructed per
    /// seed by the callers).
    ///
    /// # Errors
    /// Returns [`Error::ValidatorState`] if the validator-set lookup fails.
    pub async fn make_validators(&self, p_chain_height: u64) -> Result<Vec<ValidatorData>> {
        let set = self
            .state
            .get_validator_set(p_chain_height, self.subnet_id)
            .await
            .map_err(|e| Error::ValidatorState(format!("{e:?}")))?;

        // BTreeMap<NodeId, _> already iterates NodeId-ascending; drop the empty
        // node id (inactive ACP-77 validators).
        let validators = set
            .into_iter()
            .filter(|(id, _)| *id != NodeId::EMPTY)
            .map(|(id, v)| ValidatorData {
                id,
                weight: v.weight,
            })
            .collect();
        Ok(validators)
    }

    /// Pre-Durango proposer list for building at `block_height` (Go
    /// `Proposers`). The validator at index `i` may build after
    /// `i * WINDOW_DURATION`.
    ///
    /// # Errors
    /// Returns [`Error::WeightOverflow`] on a weight-sum overflow or
    /// [`Error::UnexpectedSamplerFailure`] if the sampler fails.
    pub async fn proposers(
        &self,
        block_height: u64,
        p_chain_height: u64,
        max_windows: u64,
    ) -> Result<Vec<NodeId>> {
        let validators = self.make_validators(p_chain_height).await?;
        proposers_from(self.chain_source, &validators, block_height, max_windows)
    }

    /// Pre-Durango delay for `validator_id` (Go `Delay`).
    ///
    /// # Errors
    /// See [`Windower::proposers`].
    pub async fn delay(
        &self,
        block_height: u64,
        p_chain_height: u64,
        validator_id: NodeId,
        max_windows: u64,
    ) -> Result<Duration> {
        if validator_id == NodeId::EMPTY {
            return Ok(WINDOW_DURATION.saturating_mul(saturating_u32(max_windows)));
        }
        let proposers = self
            .proposers(block_height, p_chain_height, max_windows)
            .await?;
        Ok(delay_for(&proposers, validator_id))
    }

    /// Post-Durango expected proposer for `slot` (Go `ExpectedProposer`).
    ///
    /// # Errors
    /// Returns [`Error::AnyoneCanPropose`] if there are no validators, or
    /// [`Error::UnexpectedSamplerFailure`] on a sampler failure.
    pub async fn expected_proposer(
        &self,
        block_height: u64,
        p_chain_height: u64,
        slot: u64,
    ) -> Result<NodeId> {
        let validators = self.make_validators(p_chain_height).await?;
        if validators.is_empty() {
            return Err(Error::AnyoneCanPropose);
        }
        expected_proposer_from(self.chain_source, &validators, block_height, slot)
    }

    /// Post-Durango minimum delay until `node_id`'s next slot, scanning up to
    /// [`MAX_LOOK_AHEAD_SLOTS`] from `start_slot` (Go `MinDelayForProposer`).
    ///
    /// # Errors
    /// Returns [`Error::AnyoneCanPropose`] if there are no validators, or
    /// [`Error::UnexpectedSamplerFailure`] on a sampler failure.
    pub async fn min_delay_for_proposer(
        &self,
        block_height: u64,
        p_chain_height: u64,
        node_id: NodeId,
        start_slot: u64,
    ) -> Result<Duration> {
        let validators = self.make_validators(p_chain_height).await?;
        if validators.is_empty() {
            return Err(Error::AnyoneCanPropose);
        }
        min_delay_for_proposer_from(
            self.chain_source,
            &validators,
            block_height,
            node_id,
            start_slot,
        )
    }
}

/// `chain_source` = the big-endian `u64` of the first 8 bytes of `chain_id`
/// (Go `wrappers.Packer{Bytes: chainID[:]}.UnpackLong()`).
#[must_use]
pub fn chain_source(chain_id: Id) -> u64 {
    let bytes = chain_id.as_bytes();
    let mut head = [0u8; 8];
    head.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(head)
}

/// `TimeToSlot` — the slot index of `now` relative to `start` (Go `TimeToSlot`).
#[must_use]
pub fn time_to_slot(start: Duration, now: Duration) -> u64 {
    if now < start {
        return 0;
    }
    let elapsed = now.saturating_sub(start);
    elapsed
        .as_secs()
        .checked_div(WINDOW_DURATION.as_secs())
        .unwrap_or(0)
}

/// Pure-sync core of [`Windower::proposers`] (the R1 gate target).
///
/// # Errors
/// Returns [`Error::WeightOverflow`] or [`Error::UnexpectedSamplerFailure`].
pub fn proposers_from(
    chain_source: u64,
    validators: &[ValidatorData],
    block_height: u64,
    max_windows: u64,
) -> Result<Vec<NodeId>> {
    let total_weight = total_weight(validators)?;

    // 32-bit MT seeded with chain_source ^ block_height.
    let mut mt = Mt19937::new();
    mt.seed(chain_source ^ block_height);
    let mut sampler = WeightedWithoutReplacementGeneric::new(Box::new(mt));
    sampler
        .initialize(&weights(validators))
        .map_err(|_| Error::WeightOverflow)?;

    let num_to_sample = usize::try_from(max_windows.min(total_weight)).unwrap_or(usize::MAX);
    let indices = sampler
        .sample(num_to_sample)
        .ok_or(Error::UnexpectedSamplerFailure)?;

    let mut out = Vec::with_capacity(indices.len());
    for index in indices {
        let v = validators
            .get(index)
            .ok_or(Error::UnexpectedSamplerFailure)?;
        out.push(v.id);
    }
    Ok(out)
}

/// The pre-Durango delay derived from a proposer list (Go `Delay` inner loop).
#[must_use]
pub fn delay_for(proposers: &[NodeId], validator_id: NodeId) -> Duration {
    let mut delay = Duration::ZERO;
    for &node_id in proposers {
        if node_id == validator_id {
            return delay;
        }
        delay = delay.saturating_add(WINDOW_DURATION);
    }
    delay
}

/// Pure-sync core of [`Windower::expected_proposer`] (the R1 gate target).
///
/// # Errors
/// Returns [`Error::WeightOverflow`] or [`Error::UnexpectedSamplerFailure`].
pub fn expected_proposer_from(
    chain_source: u64,
    validators: &[ValidatorData],
    block_height: u64,
    slot: u64,
) -> Result<NodeId> {
    // Slot is bit-reversed so that the (height, slot) seed space does not
    // collide (Go `expectedProposer`).
    let seed = chain_source ^ block_height ^ slot.reverse_bits();

    let mut mt = Mt19937_64::new();
    mt.seed(seed);
    let mut sampler = WeightedWithoutReplacementGeneric::new(Box::new(mt));
    sampler
        .initialize(&weights(validators))
        .map_err(|_| Error::WeightOverflow)?;

    let indices = sampler.sample(1).ok_or(Error::UnexpectedSamplerFailure)?;
    let index = *indices.first().ok_or(Error::UnexpectedSamplerFailure)?;
    let v = validators
        .get(index)
        .ok_or(Error::UnexpectedSamplerFailure)?;
    Ok(v.id)
}

/// Pure-sync core of [`Windower::min_delay_for_proposer`].
///
/// # Errors
/// Returns [`Error::WeightOverflow`] or [`Error::UnexpectedSamplerFailure`].
pub fn min_delay_for_proposer_from(
    chain_source: u64,
    validators: &[ValidatorData],
    block_height: u64,
    node_id: NodeId,
    start_slot: u64,
) -> Result<Duration> {
    let max_slot = start_slot.saturating_add(MAX_LOOK_AHEAD_SLOTS);
    let mut slot = start_slot;
    while slot < max_slot {
        let expected = expected_proposer_from(chain_source, validators, block_height, slot)?;
        if expected == node_id {
            return Ok(WINDOW_DURATION.saturating_mul(saturating_u32(slot)));
        }
        slot = slot.saturating_add(1);
    }
    Ok(WINDOW_DURATION.saturating_mul(saturating_u32(max_slot)))
}

/// Sums validator weights with a checked add (Go `math.Add` loop).
fn total_weight(validators: &[ValidatorData]) -> Result<u64> {
    let mut total: u64 = 0;
    for v in validators {
        total = total.checked_add(v.weight).ok_or(Error::WeightOverflow)?;
    }
    Ok(total)
}

/// The per-index weight slice in `NodeId`-sorted order.
fn weights(validators: &[ValidatorData]) -> Vec<u64> {
    validators.iter().map(|v| v.weight).collect()
}

/// Saturating `u64 -> u32` for `Duration::saturating_mul` (which takes a `u32`).
fn saturating_u32(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}
