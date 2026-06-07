// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The ACP-77 L1-validator-lifecycle tx executor
//! (`vms/platformvm/txs/executor/standard_tx_executor.go`, the L1 handlers;
//! specs 08 §6).
//!
//! [`L1TxExecutor`] is a [`Visitor`](crate::txs::Visitor) that verifies and
//! applies the five Etna L1 lifecycle txs against a [`Diff`](crate::state::diff::Diff),
//! mutating the L1-validator set through [`Diff::put_l1_validator`](crate::state::chain::Chain::put_l1_validator)
//! with the immutable-field guard ([`L1Validator::immutable_fields_are_unmodified`]):
//!
//! - [`ConvertSubnetToL1Tx`] — converts a permissioned subnet to an L1, recording
//!   the initial pay-as-you-go validators and the manager (conversion) data.
//! - [`RegisterL1ValidatorTx`] — registers an L1 validator from a verified Warp
//!   `RegisterL1Validator` message + a BLS proof-of-possession, funding its
//!   `EndAccumulatedFee` from the tx balance.
//! - [`SetL1ValidatorWeightTx`] — sets an L1 validator's weight from a verified
//!   Warp `L1ValidatorWeight` message, enforcing a monotonic nonce and refunding
//!   the remaining balance when the validator is removed.
//! - [`IncreaseL1ValidatorBalanceTx`] — tops up (and possibly reactivates) a
//!   validator's continuous-fee balance.
//! - [`DisableL1ValidatorTx`] — disables a validator (authorized by its
//!   deactivation owner) and refunds its remaining balance.
//!
//! It reuses the M4.16 shared surface: the [`Backend`] context, the
//! [`state_changes`] fee/flow helpers, and the [`subnet`] authorization helpers.
//!
//! ## Deferred seams (flagged)
//!
//! Two pieces of node-wide state are not yet on the [`Chain`] trait (owned by the
//! state-store tasks M4.13/M4.14) and so are handled as clearly-marked seams:
//!
//! - **Active-validator capacity** (`state.NumActiveL1Validators()` vs
//!   `ValidatorFeeConfig.Capacity`): there is no active-count accessor on `Chain`
//!   yet, so the `errMaxNumActiveValidators` guard is **not** enforced here. It is
//!   isolated in [`L1TxExecutor::check_active_capacity`] so the state task can
//!   wire it in additively.
//! - **Expiry replay guard** (`state.HasExpiry` / `state.PutExpiry`): there is no
//!   expiry set on `Chain` yet, so the `errWarpMessageAlreadyIssued` replay check
//!   is **not** enforced here. It is isolated in
//!   [`L1TxExecutor::check_and_record_expiry`].
//!
//! The Warp signature/quorum check is the [`WarpSignatureVerifier`] seam
//! (M4.21/M4.22); see [`crate::warp::verifier`].

use std::time::SystemTime;

use ava_secp256k1fx::{OutputOwners, TransferOutput};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::state::l1_validator::L1Validator;
use crate::txs::components::{Output, PChainOwner, TransferableOutput};
use crate::txs::{
    ConvertSubnetToL1Tx, DisableL1ValidatorTx, IncreaseL1ValidatorBalanceTx, RegisterL1ValidatorTx,
    SetL1ValidatorWeightTx, Visitor,
};
use crate::utxo::{self, Utxo};
use crate::warp::message::RegistryPayload;
use crate::warp::verifier::{self, WarpSignatureVerifier};

use super::backend::Backend;
use super::state_changes;
use super::subnet_tx_verification as subnet;

/// `RegisterL1ValidatorTxExpiryWindow` — the maximum number of seconds in the
/// future a `RegisterL1Validator` message's expiry may be (Go `day`).
pub const REGISTER_L1_VALIDATOR_TX_EXPIRY_WINDOW: u64 = 24 * 60 * 60;

/// `state.SubnetToL1Conversion` (the manager-slot value) — the recorded
/// L1-conversion of a subnet (Go `state.SubnetToL1Conversion`).
///
/// Stored in the subnet-manager slot ([`Chain::set_subnet_manager`]); the
/// `verify_l1_conversion` check decodes it to confirm a Warp message originated
/// from the expected manager chain & address. The `ConversionID` is the
/// `SubnetToL1ConversionID` hash; M4.19 stores it for parity but does not
/// re-derive it (the full conversion-id hash is a later concern).
#[derive(ava_codec::AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct SubnetConversion {
    /// `ConversionID` — the hash of the conversion data.
    #[codec]
    pub conversion_id: Id,
    /// `ChainID` — the chain hosting the subnet manager.
    #[codec]
    pub chain_id: Id,
    /// `Addr` — the subnet-manager address.
    #[codec]
    pub addr: Vec<u8>,
}

impl SubnetConversion {
    /// Marshals to the subnet-manager-slot bytes.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] on a codec write failure.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, self)
            .map_err(Error::Codec)
    }

    /// Unmarshals from the subnet-manager-slot bytes.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] on malformed bytes.
    pub fn unmarshal(bytes: &[u8]) -> Result<Self> {
        let mut v = Self::default();
        crate::txs::codec::Codec()
            .unmarshal(bytes, &mut v)
            .map_err(Error::Codec)?;
        Ok(v)
    }
}

/// `standardTxExecutor` (the L1 handlers) — a [`Visitor`] that executes the five
/// ACP-77 L1 lifecycle txs against a [`Diff`].
///
/// Construct with [`L1TxExecutor::new`], dispatch via
/// [`UnsignedTx::visit`](crate::txs::UnsignedTx::visit). The Warp
/// signature/quorum check is delegated to the injected `verifier` (the
/// M4.21/M4.22 seam).
pub struct L1TxExecutor<'a, V: WarpSignatureVerifier> {
    backend: &'a Backend,
    state: &'a mut Diff,
    /// The signed tx (for credentials / id).
    tx: &'a crate::txs::Tx,
    /// The marshaled unsigned-tx bytes (hashed by the fx for auth checks).
    unsigned_bytes: Vec<u8>,
    /// The tx id (`sha256(signed_bytes)`).
    tx_id: Id,
    /// The injected Warp signature/quorum verifier.
    warp_verifier: &'a V,
}

impl<'a, V: WarpSignatureVerifier> L1TxExecutor<'a, V> {
    /// Builds an executor over `state` for the signed `tx`. `unsigned_bytes` is
    /// the marshaled unsigned tx (the fx hashes it for owner-auth checks);
    /// `warp_verifier` is the injected signature/quorum seam.
    pub fn new(
        backend: &'a Backend,
        state: &'a mut Diff,
        tx: &'a crate::txs::Tx,
        unsigned_bytes: Vec<u8>,
        warp_verifier: &'a V,
    ) -> Self {
        Self {
            backend,
            state,
            tx,
            unsigned_bytes,
            tx_id: tx.id(),
            warp_verifier,
        }
    }

    /// The Etna gate every L1 tx shares (Go `if !upgrades.IsEtnaActivated(...)`).
    fn require_etna(&self) -> Result<()> {
        if self.backend.is_etna_activated(self.state.timestamp()) {
            Ok(())
        } else {
            Err(Error::EtnaUpgradeNotActive)
        }
    }

    /// The fee in force for this tx (fork-selected), charged on AVAX.
    fn fee(&self) -> Result<u64> {
        state_changes::fee_calculator(self.backend, self.state)
            .calculate_fee(crate::txs::fee::complexity::base_tx_complexity())
    }

    /// The shared flow check + consume/produce over an embedded base tx.
    fn verify_and_apply_base(
        &mut self,
        ins: &[crate::txs::components::TransferableInput],
        outs: &[TransferableOutput],
    ) -> Result<()> {
        let fee = self.fee()?;
        state_changes::verify_spend(self.state, ins, outs, fee, self.backend.avax_asset_id)?;
        utxo::consume(self.state, ins);
        utxo::produce(self.state, self.tx_id, outs)
    }

    /// The current chain time as a Unix-seconds `u64` (Go `uint64(t.Unix())`).
    fn chain_time_unix(&self) -> Result<u64> {
        self.state
            .timestamp()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .map_err(|_| Error::Overflow)
    }

    /// **Deferred seam (M4.13/M4.14):** verify there is room for another active
    /// L1 validator (`gas.Gas(NumActiveL1Validators()) >= Capacity`).
    ///
    /// The [`Chain`] trait has no active-count accessor yet, so this is a no-op;
    /// the state task will replace the body with the real comparison against
    /// [`crate::validators::fee::CAPACITY`].
    fn check_active_capacity(&self) -> Result<()> {
        // No `NumActiveL1Validators` on `Chain` yet — enforced by M4.13/M4.14.
        Ok(())
    }

    /// **Deferred seam (M4.13/M4.14):** verify the `RegisterL1Validator` warp
    /// message is not a replay (`HasExpiry`) and record it (`PutExpiry`).
    ///
    /// The [`Chain`] trait has no expiry set yet, so this is a no-op; the state
    /// task will wire in the `(expiry, validation_id)` replay guard.
    fn check_and_record_expiry(&mut self, _expiry: u64, _validation_id: Id) -> Result<()> {
        // No expiry set on `Chain` yet — enforced by M4.13/M4.14.
        Ok(())
    }

    /// `verifyL1Conversion` — confirm a Warp message originated from `subnet`'s
    /// recorded L1-conversion manager chain & address.
    fn verify_l1_conversion(
        &self,
        subnet_id: Id,
        expected_chain_id: Id,
        expected_address: &[u8],
    ) -> Result<()> {
        let manager_bytes = self
            .state
            .get_subnet_manager(subnet_id)
            .map_err(|_| Error::WrongWarpMessageSource)?;
        let conversion = SubnetConversion::unmarshal(&manager_bytes)?;
        if expected_chain_id != conversion.chain_id
            || expected_address != conversion.addr.as_slice()
        {
            return Err(Error::WrongWarpMessageSource);
        }
        Ok(())
    }

    /// Builds the refund UTXO produced when an active L1 validator's remaining
    /// balance is returned to its `RemainingBalanceOwner`.
    ///
    /// `output_index` is `len(tx.Outs)` (Go), `owner` the decoded remaining
    /// balance owner, `amount` the refund. Returns the marshaled UTXO bytes keyed
    /// by its input id.
    fn refund_utxo(
        &self,
        output_index: u32,
        owner: &crate::warp::message::PChainOwner,
        amount: u64,
    ) -> Result<Utxo> {
        let owners = OutputOwners::new(0, owner.threshold, owner.addresses.clone());
        Ok(Utxo {
            tx_id: self.tx_id,
            output_index,
            asset_id: self.backend.avax_asset_id,
            out: Output::Transfer(TransferOutput::new(amount, owners)),
        })
    }
}

impl<V: WarpSignatureVerifier> Visitor for L1TxExecutor<'_, V> {
    type Error = Error;

    fn convert_subnet_to_l1(&mut self, tx: &ConvertSubnetToL1Tx) -> Result<()> {
        self.require_etna()?;
        tx.base.syntactic_verify()?;

        // The issuer must control the (still-permissioned) subnet.
        subnet::verify_subnet_authorization(
            self.backend,
            self.state,
            self.tx,
            &self.unsigned_bytes,
            tx.subnet,
            &tx.subnet_auth,
        )?;

        let start_time = self.chain_time_unix()?;
        let current_fees = self.state.accrued_fees();

        for (i, vdr) in tx.validators.iter().enumerate() {
            let index = u32::try_from(i).map_err(|_| Error::Overflow)?;
            let node_id = NodeId::from_slice(&vdr.node_id).map_err(|_| Error::EmptyNodeId)?;

            let public_key = vdr.signer.key()?;
            let remaining_balance_owner = marshal_owner(&vdr.remaining_balance_owner)?;
            let deactivation_owner = marshal_owner(&vdr.deactivation_owner)?;

            let mut l1_validator = L1Validator {
                validation_id: tx.subnet.append(&[index]),
                subnet_id: tx.subnet,
                node_id,
                public_key: public_key.serialize().to_vec(),
                remaining_balance_owner,
                deactivation_owner,
                start_time,
                weight: vdr.weight,
                min_nonce: 0,
                end_accumulated_fee: 0, // If Balance is 0, this stays 0.
            };

            if vdr.balance != 0 {
                // Attempting to add an active validator.
                self.check_active_capacity()?;
                l1_validator.end_accumulated_fee = vdr
                    .balance
                    .checked_add(current_fees)
                    .ok_or(Error::Overflow)?;
            }

            self.state.put_l1_validator(l1_validator)?;
        }

        self.verify_and_apply_base(&tx.base.base.ins.clone(), &tx.base.base.outs.clone())?;

        // Record the subnet conversion in the manager slot. The conversion id is
        // not re-derived here (a later concern); the chain id + address are what
        // `verify_l1_conversion` consults.
        let conversion = SubnetConversion {
            conversion_id: Id::EMPTY,
            chain_id: tx.chain_id,
            addr: tx.address.clone(),
        };
        self.state
            .set_subnet_manager(tx.subnet, conversion.marshal()?);
        Ok(())
    }

    fn register_l1_validator(&mut self, tx: &RegisterL1ValidatorTx) -> Result<()> {
        self.require_etna()?;
        tx.base.syntactic_verify()?;

        self.verify_and_apply_base(&tx.base.base.ins.clone(), &tx.base.base.outs.clone())?;

        // Parse + structurally verify the embedded Warp message (the
        // signature/quorum step is the injected seam).
        let parsed = verifier::verify_warp_message(self.warp_verifier, &tx.message)?;
        let RegistryPayload::RegisterL1Validator(msg) = &parsed.payload else {
            return Err(Error::InvalidComponent);
        };

        // The warp message must have originated from the subnet's manager.
        self.verify_l1_conversion(
            msg.subnet_id,
            parsed.message.unsigned_message.source_chain_id,
            &parsed.addressed_call.source_address,
        )?;

        // The message must carry a valid (future, in-window) expiry.
        let current_unix = self.chain_time_unix()?;
        if msg.expiry <= current_unix {
            return Err(Error::WarpMessageExpired);
        }
        let seconds_until_expiry = msg.expiry.saturating_sub(current_unix);
        if seconds_until_expiry > REGISTER_L1_VALIDATOR_TX_EXPIRY_WINDOW {
            return Err(Error::WarpMessageNotYetAllowed);
        }

        // The validation id is `sha256` over the registry-payload bytes.
        let validation_id =
            crate::warp::message::RegisterL1Validator::validation_id(&parsed.payload_bytes);

        // Replay guard (deferred seam).
        self.check_and_record_expiry(msg.expiry, validation_id)?;

        // Verify the tx's PoP against the message's BLS public key.
        let pop = crate::signer::ProofOfPossession::new(msg.bls_public_key, tx.proof_of_possession);
        pop.verify()?;

        let node_id = NodeId::from_slice(&msg.node_id).map_err(|_| Error::EmptyNodeId)?;
        let remaining_balance_owner = marshal_message_owner(&msg.remaining_balance_owner)?;
        let deactivation_owner = marshal_message_owner(&msg.disable_owner)?;

        let mut l1_validator = L1Validator {
            validation_id,
            subnet_id: msg.subnet_id,
            node_id,
            public_key: pop.key()?.serialize().to_vec(),
            remaining_balance_owner,
            deactivation_owner,
            start_time: current_unix,
            weight: msg.weight,
            min_nonce: 0,
            end_accumulated_fee: 0, // If Balance is 0, this stays 0.
        };

        if tx.balance != 0 {
            self.check_active_capacity()?;
            let current_fees = self.state.accrued_fees();
            l1_validator.end_accumulated_fee = tx
                .balance
                .checked_add(current_fees)
                .ok_or(Error::Overflow)?;
        }

        self.state.put_l1_validator(l1_validator)
    }

    fn set_l1_validator_weight(&mut self, tx: &SetL1ValidatorWeightTx) -> Result<()> {
        self.require_etna()?;
        tx.base.syntactic_verify()?;

        self.verify_and_apply_base(&tx.base.base.ins.clone(), &tx.base.base.outs.clone())?;

        // Parse + structurally verify the embedded Warp message.
        let parsed = verifier::verify_warp_message(self.warp_verifier, &tx.message)?;
        let RegistryPayload::L1ValidatorWeight(msg) = &parsed.payload else {
            return Err(Error::InvalidComponent);
        };

        // The message must carry a non-stale nonce for a current validator.
        let mut l1_validator = self
            .state
            .get_l1_validator(msg.validation_id)
            .map_err(|_| Error::CouldNotLoadL1Validator)?;
        if msg.nonce < l1_validator.min_nonce {
            return Err(Error::WarpMessageContainsStaleNonce);
        }

        // The warp message must have originated from the subnet's manager.
        self.verify_l1_conversion(
            l1_validator.subnet_id,
            parsed.message.unsigned_message.source_chain_id,
            &parsed.addressed_call.source_address,
        )?;

        // Removing the validator?
        if msg.weight == 0 {
            // Refuse to remove the last validator of the converted subnet.
            let total_weight = self
                .state
                .weight_of_l1_validators(l1_validator.subnet_id)
                .map_err(|_| Error::CouldNotLoadL1Validator)?;
            if total_weight == l1_validator.weight {
                return Err(Error::RemovingLastValidator);
            }

            // Refund the remaining balance of an active validator.
            if l1_validator.end_accumulated_fee != 0 {
                let owner = unmarshal_message_owner(&l1_validator.remaining_balance_owner)?;
                let accrued_fees = self.state.accrued_fees();
                if l1_validator.end_accumulated_fee <= accrued_fees {
                    // Unreachable in a consistent state; guards against minting.
                    return Err(Error::StateCorruption);
                }
                let remaining_balance = l1_validator
                    .end_accumulated_fee
                    .saturating_sub(accrued_fees);
                let output_index =
                    u32::try_from(tx.base.base.outs.len()).map_err(|_| Error::Overflow)?;
                let utxo = self.refund_utxo(output_index, &owner, remaining_balance)?;
                self.state.add_utxo(utxo.input_id(), utxo.marshal()?);
            }
        }

        // For a removal (`weight == 0`) the nonce increment may overflow, but the
        // validator is being removed and the nonce no longer matters; for a
        // non-removal `msg.Verify()` guarantees `nonce < MaxUint64`.
        l1_validator.min_nonce = msg.nonce.saturating_add(1);
        l1_validator.weight = msg.weight;
        self.state.put_l1_validator(l1_validator)
    }

    fn increase_l1_validator_balance(&mut self, tx: &IncreaseL1ValidatorBalanceTx) -> Result<()> {
        self.require_etna()?;
        tx.base.syntactic_verify()?;

        self.verify_and_apply_base(&tx.base.base.ins.clone(), &tx.base.base.outs.clone())?;

        let mut l1_validator = self.state.get_l1_validator(tx.validation_id)?;

        // If currently inactive, we are activating it.
        if l1_validator.end_accumulated_fee == 0 {
            self.check_active_capacity()?;
            l1_validator.end_accumulated_fee = self.state.accrued_fees();
        }
        l1_validator.end_accumulated_fee = l1_validator
            .end_accumulated_fee
            .checked_add(tx.balance)
            .ok_or(Error::Overflow)?;

        self.state.put_l1_validator(l1_validator)
    }

    fn disable_l1_validator(&mut self, tx: &DisableL1ValidatorTx) -> Result<()> {
        self.require_etna()?;
        tx.base.syntactic_verify()?;

        let l1_validator = self
            .state
            .get_l1_validator(tx.validation_id)
            .map_err(|_| Error::CouldNotLoadL1Validator)?;

        // The deactivation owner must authorize the disable.
        let disable_owner = unmarshal_message_owner(&l1_validator.deactivation_owner)?;
        let owners = OutputOwners::new(0, disable_owner.threshold, disable_owner.addresses.clone());
        subnet::verify_authorization(
            self.backend,
            self.tx,
            &self.unsigned_bytes,
            &owners,
            &tx.disable_auth,
        )?;

        self.verify_and_apply_base(&tx.base.base.ins.clone(), &tx.base.base.outs.clone())?;

        // Nothing to refund if already disabled.
        if l1_validator.end_accumulated_fee == 0 {
            return Ok(());
        }

        let remaining_balance_owner =
            unmarshal_message_owner(&l1_validator.remaining_balance_owner)?;
        let accrued_fees = self.state.accrued_fees();
        if l1_validator.end_accumulated_fee <= accrued_fees {
            // Unreachable in a consistent state; guards against minting.
            return Err(Error::StateCorruption);
        }
        let remaining_balance = l1_validator
            .end_accumulated_fee
            .saturating_sub(accrued_fees);
        let output_index = u32::try_from(tx.base.base.outs.len()).map_err(|_| Error::Overflow)?;
        let utxo = self.refund_utxo(output_index, &remaining_balance_owner, remaining_balance)?;
        self.state.add_utxo(utxo.input_id(), utxo.marshal()?);

        // Disable the validator.
        let mut disabled = l1_validator;
        disabled.end_accumulated_fee = 0;
        self.state.put_l1_validator(disabled)
    }
}

/// Marshals a tx-component [`PChainOwner`] to the `secp256k1fx.OutputOwners`
/// bytes stored as a validator's owner field (Go `txs.Codec.Marshal(&owner)`).
fn marshal_owner(owner: &PChainOwner) -> Result<Vec<u8>> {
    let owners = OutputOwners::new(0, owner.threshold, owner.addresses.clone());
    let component = crate::txs::components::Owner::Secp256k1(owners);
    crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &component)
        .map_err(Error::Codec)
}

/// Marshals a warp-message [`crate::warp::message::PChainOwner`] to the owner
/// bytes stored on the L1 validator.
fn marshal_message_owner(owner: &crate::warp::message::PChainOwner) -> Result<Vec<u8>> {
    let owners = OutputOwners::new(0, owner.threshold, owner.addresses.clone());
    let component = crate::txs::components::Owner::Secp256k1(owners);
    crate::txs::codec::Codec()
        .marshal(crate::CODEC_VERSION, &component)
        .map_err(Error::Codec)
}

/// Decodes the stored owner bytes into a warp-message [`PChainOwner`] (threshold
/// + addresses), the shape the refund / disable-auth paths consume.
fn unmarshal_message_owner(bytes: &[u8]) -> Result<crate::warp::message::PChainOwner> {
    let owners = subnet::decode_owner(bytes)?;
    Ok(crate::warp::message::PChainOwner {
        threshold: owners.threshold,
        addresses: owners.addrs,
    })
}

#[cfg(test)]
#[path = "l1_executor_tests.rs"]
mod l1_lifecycle;
