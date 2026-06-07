// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `txs.AddPermissionlessValidatorTx` (type_id 25) — add a permissionless
//! validator to a subnet or the Primary Network (specs 08 §2.2).

use std::cell::OnceCell;

use ava_codec::AvaCodec;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::math as safemath;

use crate::Error;
use crate::reward::PERCENT_DENOMINATOR;
use crate::signer::Signer;
use crate::txs::base_tx::BaseTx;
use crate::txs::components::{self, Owner, TransferableOutput};
use crate::txs::validator::Validator;

/// `txs.AddPermissionlessValidatorTx`.
#[derive(AvaCodec, Clone, Debug, Default)]
pub struct AddPermissionlessValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Describes the validator (node id, staking window, weight).
    #[codec]
    pub validator: Validator,
    /// ID of the subnet this validator is validating.
    #[codec]
    pub subnet: Id,
    /// The BLS signer (`signer.ProofOfPossession` on the Primary Network, else
    /// `signer.Empty`).
    #[codec]
    pub signer: Signer,
    /// Where to send staked tokens when done validating.
    #[codec]
    pub stake_outs: Vec<TransferableOutput>,
    /// Where to send validation rewards when done validating.
    #[codec]
    pub validator_rewards_owner: Owner,
    /// Where to send delegation rewards when done validating.
    #[codec]
    pub delegator_rewards_owner: Owner,
    /// Fee this validator charges delegators, in millionths.
    #[codec]
    pub delegation_shares: u32,
    /// `SyntacticallyVerified` — non-serialized memo.
    pub verified: OnceCell<()>,
}

impl PartialEq for AddPermissionlessValidatorTx {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base
            && self.validator == other.validator
            && self.subnet == other.subnet
            && self.signer == other.signer
            && self.stake_outs == other.stake_outs
            && self.validator_rewards_owner == other.validator_rewards_owner
            && self.delegator_rewards_owner == other.delegator_rewards_owner
            && self.delegation_shares == other.delegation_shares
    }
}

impl Eq for AddPermissionlessValidatorTx {}

impl AddPermissionlessValidatorTx {
    /// `AddPermissionlessValidatorTx.SyntacticVerify` (specs 08 §2.2):
    /// non-empty node id, non-empty stake, `delegation_shares <=
    /// PERCENT_DENOMINATOR`, base verify, validator/signer/owners verify, BLS
    /// key present iff Primary Network, stake outputs sorted, single asset, and
    /// summing to `validator.wght`.
    ///
    /// # Errors
    /// Returns the matching [`Error`] variant on any failed check.
    pub fn syntactic_verify(&self) -> Result<(), Error> {
        if self.verified.get().is_some() {
            return Ok(());
        }
        if self.validator.node_id == NodeId::EMPTY {
            return Err(Error::EmptyNodeId);
        }
        if self.stake_outs.is_empty() {
            return Err(Error::NoStake);
        }
        if u64::from(self.delegation_shares) > PERCENT_DENOMINATOR {
            return Err(Error::TooManyShares);
        }
        self.base.syntactic_verify()?;
        self.validator.verify()?;
        self.signer.verify()?;
        self.validator_rewards_owner.verify()?;
        self.delegator_rewards_owner.verify()?;

        let has_key = self.signer.has_key();
        let is_primary = self.subnet == Id::EMPTY;
        if has_key != is_primary {
            return Err(Error::InvalidSigner);
        }

        for out in &self.stake_outs {
            out.verify()?;
        }
        let (first, rest) = self.stake_outs.split_first().ok_or(Error::NoStake)?;
        let staked_asset = first.asset_id();
        let mut total: u64 = first.amount();
        for out in rest {
            total = safemath::add(total, out.amount()).map_err(|_| Error::Overflow)?;
            if out.asset_id() != staked_asset {
                return Err(Error::MultipleStakedAssets);
            }
        }
        if !components::is_sorted_transferable_outputs(&self.stake_outs) {
            return Err(Error::OutputsNotSorted);
        }
        if total != self.validator.wght {
            return Err(Error::ValidatorWeightMismatch);
        }
        let _ = self.verified.set(());
        Ok(())
    }
}
