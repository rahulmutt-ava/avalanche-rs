// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Helicon auto-renew lifecycle txs (specs 08 §2.2):
//! `AddAutoRenewedValidatorTx` (40), `SetAutoRenewedValidatorConfigTx` (41),
//! `RewardAutoRenewedValidatorTx` (42).

use ava_codec::AvaCodec;
use ava_crypto::bls;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::math as safemath;

use crate::Error;
use crate::reward::PERCENT_DENOMINATOR;
use crate::signer::Signer;
use crate::txs::Priority;
use crate::txs::base_tx::BaseTx;
use crate::txs::components::{self, Auth, Owner, TransferableOutput};

/// `txs.AddAutoRenewedValidatorTx` (type_id 40).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct AddAutoRenewedValidatorTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// Node ID of the validator (raw bytes; length-prefixed).
    #[codec]
    pub validator_node_id: Vec<u8>,
    /// The BLS signer for this validator.
    #[codec]
    pub signer: Signer,
    /// Where to send staked tokens when done validating.
    #[codec]
    pub stake_outs: Vec<TransferableOutput>,
    /// Where to send validation rewards.
    #[codec]
    pub validator_rewards_owner: Owner,
    /// Where to send delegation rewards.
    #[codec]
    pub delegator_rewards_owner: Owner,
    /// Who is authorized to manage this validator.
    #[codec]
    pub validator_authority: Owner,
    /// Fee this validator charges delegators, in millionths.
    #[codec]
    pub delegation_shares: u32,
    /// Percentage of rewards to restake at each cycle end, in millionths.
    #[codec]
    pub auto_compound_reward_shares: u32,
    /// The validation cycle duration, in seconds.
    #[codec]
    pub period: u64,
}

impl AddAutoRenewedValidatorTx {
    /// `SubnetID` — always the Primary Network ([`Id::EMPTY`]).
    #[must_use]
    pub fn subnet_id(&self) -> Id {
        Id::EMPTY
    }

    /// `NodeID` — the validator's node id, parsed from [`Self::validator_node_id`].
    ///
    /// # Errors
    /// Returns [`Error::EmptyNodeId`] if the byte slice is not a valid node id
    /// (must only be called on a syntactically-verified tx).
    pub fn node_id(&self) -> Result<NodeId, Error> {
        NodeId::from_slice(&self.validator_node_id).map_err(|_| Error::EmptyNodeId)
    }

    /// `Weight` — the validator's stake weight (sum of the staked outputs).
    ///
    /// # Errors
    /// Returns [`Error::Overflow`] if the stake outputs overflow `u64` or
    /// [`Error::NoStake`] if there are none.
    pub fn weight(&self) -> Result<u64, Error> {
        let (first, rest) = self.stake_outs.split_first().ok_or(Error::NoStake)?;
        let mut total = first.amount();
        for out in rest {
            total = safemath::add(total, out.amount()).map_err(|_| Error::Overflow)?;
        }
        Ok(total)
    }

    /// `Shares` — the delegation fee, in millionths.
    #[must_use]
    pub fn shares(&self) -> u32 {
        self.delegation_shares
    }

    /// `CurrentPriority` — auto-renewed validators join the current set as
    /// primary-network validators.
    #[must_use]
    pub fn current_priority(&self) -> Priority {
        Priority::PrimaryNetworkValidatorCurrent
    }

    /// `PublicKey` — the validator's BLS key (always present; the signer is
    /// mandatory for an auto-renewed validator).
    ///
    /// # Errors
    /// Propagates [`Signer::key`].
    pub fn public_key(&self) -> Result<Option<bls::PublicKey>, Error> {
        self.signer.key()
    }

    /// `AddAutoRenewedValidatorTx.SyntacticVerify` (specs 08 §2.2): non-empty
    /// stake, `delegation_shares`/`auto_compound_reward_shares <=
    /// PERCENT_DENOMINATOR`, non-zero `period`, valid node id, base verify,
    /// signer/owners/authority verify, a present signer, single AVAX staked
    /// asset, sorted stake outputs.
    ///
    /// `avax_asset_id` is the chain's AVAX asset id (`ctx.AVAXAssetID`) used for
    /// the staked-asset check.
    ///
    /// # Errors
    /// Returns the matching [`Error`] variant on any failed check.
    pub fn syntactic_verify(&self, avax_asset_id: Id) -> Result<(), Error> {
        if self.stake_outs.is_empty() {
            return Err(Error::NoStake);
        }
        if u64::from(self.delegation_shares) > PERCENT_DENOMINATOR {
            return Err(Error::TooManyShares);
        }
        if u64::from(self.auto_compound_reward_shares) > PERCENT_DENOMINATOR {
            return Err(Error::TooManyShares);
        }
        if self.period == 0 {
            return Err(Error::StakeTooShort);
        }
        // Valid node id.
        self.node_id()?;

        self.base.syntactic_verify()?;
        self.signer.verify()?;
        self.validator_rewards_owner.verify()?;
        self.delegator_rewards_owner.verify()?;
        self.validator_authority.verify()?;

        // The signer (BLS key) is mandatory for an auto-renewed validator.
        if !self.signer.has_key() {
            return Err(Error::InvalidSigner);
        }

        for out in &self.stake_outs {
            out.verify()?;
            if out.asset_id() != avax_asset_id {
                return Err(Error::WrongStakedAssetId);
            }
        }
        if !components::is_sorted_transferable_outputs(&self.stake_outs) {
            return Err(Error::OutputsNotSorted);
        }
        // Folding the weight surfaces a stake-output overflow.
        self.weight()?;
        Ok(())
    }
}

/// `txs.SetAutoRenewedValidatorConfigTx` (type_id 41).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SetAutoRenewedValidatorConfigTx {
    /// Metadata, inputs and outputs.
    #[codec]
    pub base: BaseTx,
    /// ID of the tx that created the auto-renewed validator.
    #[codec]
    pub tx_id: Id,
    /// Authorizes this validator to be updated.
    #[codec]
    pub auth: Auth,
    /// Percentage of rewards to restake at each cycle end, in millionths.
    #[codec]
    pub auto_compound_reward_shares: u32,
    /// Period for the next cycle (in seconds); 0 stops at the current cycle end.
    #[codec]
    pub period: u64,
}

impl SetAutoRenewedValidatorConfigTx {
    /// `SetAutoRenewedValidatorConfigTx.SyntacticVerify` (specs 08 §2.2):
    /// non-empty `tx_id`, `auto_compound_reward_shares <= PERCENT_DENOMINATOR`,
    /// base verify, and auth verify.
    ///
    /// # Errors
    /// Returns the matching [`Error`] variant on any failed check.
    pub fn syntactic_verify(&self) -> Result<(), Error> {
        if self.tx_id == Id::EMPTY {
            // `errMissingTxID`.
            return Err(Error::WrongTxType);
        }
        if u64::from(self.auto_compound_reward_shares) > PERCENT_DENOMINATOR {
            return Err(Error::TooManyShares);
        }
        self.base.syntactic_verify()?;
        self.auth.verify()?;
        Ok(())
    }
}

/// `txs.RewardAutoRenewedValidatorTx` (type_id 42). No embedded `BaseTx`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct RewardAutoRenewedValidatorTx {
    /// ID of the tx that created the validator being rewarded.
    #[codec]
    pub tx_id: Id,
    /// End time of the validation cycle.
    #[codec]
    pub timestamp: u64,
}
