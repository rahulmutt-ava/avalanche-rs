// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `state.Chain` read+write surface and the `state.Versions` resolver
//! (`vms/platformvm/state/{chain,versions}.go`, specs 08 §3.1).
//!
//! [`Chain`] is the trait shared by the persisted base [`State`](super::state::State)
//! and the in-memory [`Diff`](super::diff::Diff) overlay: every accepted block
//! carries a `Diff`, and on accept the diff chain is applied down to `State`.
//! [`Versions`] resolves a block ID to the `Chain` view at that block.
//!
//! ## UTXO representation (as-built, M4.13)
//!
//! The spec sketch types the UTXO surface as `avax::Utxo`. `avax::Utxo` carries
//! an `Arc<dyn State>` fx payload that is not yet codec-serializable in isolation
//! (the fx-registered UTXO handler is M4.15), so M4.13 stores UTXOs as their
//! **opaque codec bytes** ([`UtxoBytes`]) — exactly the cross-chain / shared-memory
//! byte layout that *is* protocol-relevant (specs 08 §3.2). The typed
//! `avax::Utxo` round-trip is layered on by M4.15.

use std::time::SystemTime;

use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::Result;
use crate::state::l1_validator::L1Validator;
use crate::state::staker::Staker;
use crate::txs::fee::gas::GasState;

/// The opaque codec bytes of an `avax.UTXO` (the protocol-relevant value layout).
///
/// See the module docs for why M4.13 stores UTXOs as bytes rather than the typed
/// `avax::Utxo`.
pub type UtxoBytes = Vec<u8>;

/// `state.Chain` — the read+write surface over P-Chain state, shared by the
/// persisted [`State`](super::state::State) base and the [`Diff`](super::diff::Diff)
/// overlay (specs 08 §3.1).
///
/// Mutators that cannot fail take `&mut self` and return `()`; reads and fallible
/// mutators return [`Result`]. Absent keys surface as
/// [`Error::NotFound`](crate::error::Error) via the wrapped database error where a
/// Go method returns `database.ErrNotFound`.
pub trait Chain: Send + Sync {
    // ----- chain time -----

    /// `GetTimestamp` — the current chain time.
    fn timestamp(&self) -> SystemTime;
    /// `SetTimestamp` — set the current chain time.
    fn set_timestamp(&mut self, t: SystemTime);

    // ----- supply -----

    /// `GetCurrentSupply` — the current supply of `subnet` (Primary Network is
    /// [`Id::EMPTY`]).
    ///
    /// # Errors
    /// Returns an error if the supply cannot be read.
    fn current_supply(&self, subnet: Id) -> Result<u64>;
    /// `SetCurrentSupply` — set the current supply of `subnet`.
    fn set_current_supply(&mut self, subnet: Id, supply: u64);

    // ----- fee / gas state -----

    /// `GetFeeState` — the ACP-103 gas meter `(capacity, excess)`.
    fn fee_state(&self) -> GasState;
    /// `SetFeeState` — set the ACP-103 gas meter.
    fn set_fee_state(&mut self, s: GasState);

    /// `GetL1ValidatorExcess` — the ACP-77 validator-fee excess accumulator.
    fn l1_validator_excess(&self) -> u64;
    /// `SetL1ValidatorExcess` — set the ACP-77 validator-fee excess accumulator.
    fn set_l1_validator_excess(&mut self, excess: u64);

    /// `GetAccruedFees` — the accrued continuous-fee total.
    fn accrued_fees(&self) -> u64;
    /// `SetAccruedFees` — set the accrued continuous-fee total.
    fn set_accrued_fees(&mut self, v: u64);

    // ----- UTXOs (bytes; see module docs) -----

    /// `GetUTXO` — the UTXO bytes for `id`.
    ///
    /// # Errors
    /// Returns [`Error::Database`](crate::error::Error) wrapping
    /// `database.ErrNotFound` when the UTXO is absent or deleted.
    fn get_utxo(&self, id: Id) -> Result<UtxoBytes>;
    /// `AddUTXO` — insert/overwrite the UTXO `id → bytes`.
    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes);
    /// `DeleteUTXO` — remove `id` from the UTXO set.
    fn delete_utxo(&mut self, id: Id);

    // ----- current stakers -----

    /// `GetCurrentValidator` — the current validator for `(subnet, node)`.
    ///
    /// # Errors
    /// Returns an error when no such current validator exists.
    fn get_current_validator(&self, subnet: Id, node: NodeId) -> Result<Staker>;
    /// `PutCurrentValidator` — add/replace a current validator.
    ///
    /// # Errors
    /// Returns an error if the staker is malformed (e.g. not a current priority).
    fn put_current_validator(&mut self, s: Staker) -> Result<()>;
    /// `DeleteCurrentValidator` — remove a current validator.
    fn delete_current_validator(&mut self, s: &Staker);
    /// `PutCurrentDelegator` — add a current delegator.
    fn put_current_delegator(&mut self, s: Staker);
    /// `DeleteCurrentDelegator` — remove a current delegator.
    fn delete_current_delegator(&mut self, s: &Staker);
    /// `GetCurrentStakerIterator` — every current staker, in `Staker` (Less)
    /// order.
    fn current_stakers(&self) -> Vec<Staker>;

    /// `GetStakingInfo` — the mutable [`StakingInfo`] of the validator `(subnet,
    /// node)` (ACP-236 auto-renew, specs 08 §3.4).
    ///
    /// # Errors
    /// Returns [`Error::Database`](crate::error::Error) wrapping
    /// `database.ErrNotFound` when no such current validator exists.
    fn get_staking_info(
        &self,
        subnet: Id,
        node: NodeId,
    ) -> Result<crate::state::metadata_validator::StakingInfo>;
    /// `SetStakingInfo` — replace the mutable [`StakingInfo`] of the validator
    /// `(subnet, node)`.
    ///
    /// # Errors
    /// Returns [`Error::Database`](crate::error::Error) wrapping
    /// `database.ErrNotFound` when no such current validator exists.
    fn set_staking_info(
        &mut self,
        subnet: Id,
        node: NodeId,
        info: crate::state::metadata_validator::StakingInfo,
    ) -> Result<()>;

    // ----- pending stakers -----

    /// `PutPendingValidator` — add a pending validator.
    ///
    /// # Errors
    /// Returns an error if the staker is malformed (e.g. not a pending priority).
    fn put_pending_validator(&mut self, s: Staker) -> Result<()>;
    /// `DeletePendingValidator` — remove a pending validator.
    fn delete_pending_validator(&mut self, s: &Staker);
    /// `PutPendingDelegator` — add a pending delegator.
    fn put_pending_delegator(&mut self, s: Staker);
    /// `DeletePendingDelegator` — remove a pending delegator.
    fn delete_pending_delegator(&mut self, s: &Staker);
    /// `GetPendingStakerIterator` — every pending staker, in `Staker` (Less)
    /// order.
    fn pending_stakers(&self) -> Vec<Staker>;

    // ----- ACP-77 L1 validators -----

    /// `GetL1Validator` — the L1 validator keyed by `validation_id`.
    ///
    /// # Errors
    /// Returns an error when the L1 validator is absent.
    fn get_l1_validator(&self, validation_id: Id) -> Result<L1Validator>;
    /// `PutL1Validator` — add/replace an L1 validator (immutable-field guarded by
    /// the caller via [`L1Validator::immutable_fields_are_unmodified`]).
    ///
    /// # Errors
    /// Returns an error if the L1 validator is malformed.
    fn put_l1_validator(&mut self, v: L1Validator) -> Result<()>;
    /// `WeightOfL1Validators` — the total active weight of `subnet`'s L1
    /// validators.
    ///
    /// # Errors
    /// Returns an error if the weight cannot be summed.
    fn weight_of_l1_validators(&self, subnet: Id) -> Result<u64>;
    /// `GetActiveL1ValidatorsIterator` — every *active* L1 validator
    /// ([`L1Validator::is_active`](crate::state::l1_validator::L1Validator::is_active)),
    /// in canonical `(EndAccumulatedFee, ValidationID)` order
    /// ([`L1Validator::compare`](crate::state::l1_validator::L1Validator::compare)).
    ///
    /// This is the order the ACP-77 continuous-fee charging walks
    /// (`state/state.go` `GetActiveL1ValidatorsIterator`); the length of the
    /// returned slice is Go's `NumActiveL1Validators`.
    fn active_l1_validators(&self) -> Vec<L1Validator>;

    // ----- subnets / chains / owners / managers -----

    /// `GetSubnetIDs` — the set of created subnet IDs.
    fn subnets(&self) -> Vec<Id>;
    /// `AddSubnet` — record a created subnet.
    fn add_subnet(&mut self, subnet: Id);
    /// `GetSubnetOwner` — the owner bytes of `subnet`.
    ///
    /// # Errors
    /// Returns an error when no owner is recorded.
    fn get_subnet_owner(&self, subnet: Id) -> Result<Vec<u8>>;
    /// `SetSubnetOwner` — set the owner bytes of `subnet`.
    fn set_subnet_owner(&mut self, subnet: Id, owner: Vec<u8>);
    /// `GetSubnetManager` — the L1-conversion (manager) bytes of `subnet`.
    ///
    /// # Errors
    /// Returns an error when no manager is recorded.
    fn get_subnet_manager(&self, subnet: Id) -> Result<Vec<u8>>;
    /// `SetSubnetManager` — set the L1-conversion (manager) bytes of `subnet`.
    fn set_subnet_manager(&mut self, subnet: Id, manager: Vec<u8>);
    /// `GetChains` — the chain IDs created under `subnet`.
    fn chains(&self, subnet: Id) -> Vec<Id>;
    /// `AddChain` — record a chain created under `subnet`.
    fn add_chain(&mut self, subnet: Id, chain: Id);

    // ----- reward UTXOs -----

    /// `GetRewardUTXOs` — the reward UTXO byte blobs for the staker tx `tx_id`.
    fn get_reward_utxos(&self, tx_id: Id) -> Vec<UtxoBytes>;
    /// `AddRewardUTXO` — append a reward UTXO blob under the staker tx `tx_id`.
    fn add_reward_utxo(&mut self, tx_id: Id, utxo: UtxoBytes);

    // ----- tx store -----

    /// `GetTx` — the **signed-tx codec bytes** stored for `tx_id` (Go
    /// `state.GetTx`; the persisted status field is not yet tracked — 08 §3.2
    /// note). Returns the opaque bytes so the caller can
    /// [`Tx::parse`](crate::txs::Tx::parse) it; this is what the block manager's
    /// reward-staker resolver reads to recover a staker's originating tx.
    ///
    /// # Errors
    /// Returns [`Error::Database`](crate::error::Error) wrapping
    /// `database.ErrNotFound` when the tx is absent.
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>>;
    /// `AddTx` — store the signed-tx bytes under `tx_id` (the acceptor writes a
    /// block's txs here so the reward path can later resolve a staker tx).
    fn add_tx(&mut self, tx_id: Id, tx_bytes: Vec<u8>);
}

/// `state.Versions` — resolves a block ID to the `Chain` view at that block
/// (specs 08 §3.1). Implemented by the block manager (M4.20) and by tests.
pub trait Versions: Send + Sync {
    /// `GetState` — the `Chain` view at `block_id`, or `None` if unknown.
    fn get_state(&self, block_id: Id) -> Option<std::sync::Arc<dyn Chain>>;
}
