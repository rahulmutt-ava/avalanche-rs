// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-chain wallet facade — port of `wallet/chain/p/wallet`
//! (`wallet.go` + `backend.go` + `backend_visitor.go`) and the issuing client
//! (`wallet/chain/p/client.go`).
//!
//! [`Wallet::issue_base_tx`] (and friends) = build → sign → submit over the
//! [`PChainClient`] seam → poll for acceptance (unless
//! [`TxOption::AssumeDecided`]) → record in the [`Backend`]
//! (`Backend.AcceptTx`: consume the spent UTXOs, add the produced ones —
//! including exported UTXOs into the destination chain's view of the shared
//! store — and track new owners).

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use ava_platformvm::signer::Signer as PopSigner;
use ava_platformvm::txs::components::{Owner, TransferableOutput};
use ava_platformvm::txs::{ConvertSubnetToL1Validator, SubnetValidator, UnsignedTx};
use ava_platformvm::utxo::Utxo;
use ava_secp256k1fx::OutputOwners;
use ava_types::id::Id;
use ava_types::node_id::NodeId;

use super::backend::Backend as StateBackend;
use super::builder::{Builder, PBuilder};
use super::signer::{SignedTx, Signer};
use super::{Context, PLATFORM_CHAIN_ID};
use crate::client::PChainClient;
use crate::common::utxos::{UtxoStore, XcUtxo, p_output_to_avm};
use crate::error::{Error, Result};
use crate::keychain::Keychain;
use crate::options::{Options, TxOption, union_options};

/// `wallet.Backend` — the mutable wallet state: the (shared) cross-chain UTXO
/// store plus the owner registry, updated on every accepted tx
/// (`backend.AcceptTx`).
pub struct Backend {
    utxos: Arc<UtxoStore>,
    owners: RwLock<BTreeMap<Id, OutputOwners>>,
}

impl Backend {
    /// `wallet.NewBackend(chainUTXOs, owners)`.
    #[must_use]
    pub fn new(utxos: Arc<UtxoStore>, owners: BTreeMap<Id, OutputOwners>) -> Self {
        Self {
            utxos,
            owners: RwLock::new(owners),
        }
    }

    fn set_owner(&self, owner_id: Id, owner: OutputOwners) {
        self.owners
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(owner_id, owner);
    }

    /// `Backend.AcceptTx` — records an issued tx: removes the consumed UTXOs,
    /// adds the produced ones (exported UTXOs land in the destination chain's
    /// view of the shared store) and tracks newly created owners
    /// (`backend_visitor.go`).
    ///
    /// # Errors
    /// [`Error::UnsupportedTxType`] for non-wallet txs
    /// (`AdvanceTime`/`Reward*`); [`Error::Codec`]/[`Error::Warp`] on an
    /// unparsable `RegisterL1Validator` message; [`Error::UnknownOutputType`]
    /// if an exported output is not a transfer output.
    pub fn accept_tx(&self, tx: &SignedTx) -> Result<()> {
        let tx_id = tx.tx_id;
        match &tx.unsigned {
            UnsignedTx::AdvanceTime(_)
            | UnsignedTx::RewardValidator(_)
            | UnsignedTx::RewardAutoRenewedValidator(_) => return Err(Error::UnsupportedTxType),
            UnsignedTx::CreateSubnet(utx) => {
                let Owner::Secp256k1(owner) = &utx.owner;
                self.set_owner(tx_id, owner.clone());
            }
            UnsignedTx::TransferSubnetOwnership(utx) => {
                let Owner::Secp256k1(owner) = &utx.owner;
                self.set_owner(utx.subnet, owner.clone());
            }
            UnsignedTx::ConvertSubnetToL1(utx) => {
                for (i, vdr) in utx.validators.iter().enumerate() {
                    let index = u32::try_from(i).map_err(|_| Error::Overflow)?;
                    self.set_owner(utx.subnet.append(&[index]), deactivation_owner(vdr));
                }
            }
            UnsignedTx::RegisterL1Validator(utx) => {
                let msg = ava_warp::Message::parse(&utx.message)?;
                let call = ava_warp::payload::AddressedCall::parse(&msg.unsigned_message.payload)?;
                let ava_warp::message::RegistryPayload::RegisterL1Validator(register) =
                    ava_warp::message::RegistryPayload::parse(&call.payload)?
                else {
                    return Err(Error::UnsupportedTxType);
                };
                let validation_id =
                    ava_warp::message::RegisterL1Validator::validation_id(&call.payload);
                self.set_owner(
                    validation_id,
                    OutputOwners::new(
                        0,
                        register.disable_owner.threshold,
                        register.disable_owner.addresses.clone(),
                    ),
                );
            }
            UnsignedTx::AddAutoRenewedValidator(utx) => {
                let Owner::Secp256k1(owner) = &utx.validator_authority;
                self.set_owner(tx_id, owner.clone());
            }
            UnsignedTx::Import(utx) => {
                for input in &utx.imported_inputs {
                    self.utxos.remove_p(utx.source_chain, input.input_id());
                }
            }
            UnsignedTx::Export(utx) => {
                let base_outs = tx.unsigned.outputs().len();
                for (i, out) in utx.exported_outputs.iter().enumerate() {
                    let index = base_outs
                        .checked_add(i)
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or(Error::Overflow)?;
                    self.utxos.add_xc(
                        PLATFORM_CHAIN_ID,
                        utx.destination_chain,
                        XcUtxo {
                            tx_id,
                            output_index: index,
                            asset_id: out.asset_id,
                            out: p_output_to_avm(&out.out)?,
                        },
                    );
                }
            }
            _ => {}
        }

        // `baseTx`: remove the consumed local inputs...
        for input in tx.unsigned.inputs() {
            self.utxos.remove_p(PLATFORM_CHAIN_ID, input.input_id());
        }
        // ...and add every produced output (`tx.UTXOs()`).
        for (i, out) in tx.unsigned.outputs().iter().enumerate() {
            let output_index = u32::try_from(i).map_err(|_| Error::Overflow)?;
            self.utxos.add_p(
                PLATFORM_CHAIN_ID,
                Utxo {
                    tx_id,
                    output_index,
                    asset_id: out.asset_id,
                    out: out.out.clone(),
                },
            );
        }
        Ok(())
    }
}

/// `ConvertSubnetToL1Validator.DeactivationOwner` as `OutputOwners`.
fn deactivation_owner(vdr: &ConvertSubnetToL1Validator) -> OutputOwners {
    OutputOwners::new(
        0,
        vdr.deactivation_owner.threshold,
        vdr.deactivation_owner.addresses.clone(),
    )
}

impl StateBackend for Backend {
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo> {
        self.utxos.p_utxos(source_chain_id)
    }

    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo> {
        self.utxos.get_p(source_chain_id, utxo_id)
    }

    fn get_owner(&self, owner_id: Id) -> Option<OutputOwners> {
        self.owners
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&owner_id)
            .cloned()
    }
}

/// `pwallet.Wallet` — build + sign + issue + record (`wallet.go`).
#[derive(Clone)]
pub struct Wallet {
    client: Arc<dyn PChainClient>,
    backend: Arc<Backend>,
    keychain: Arc<Keychain>,
    context: Context,
    default_options: Vec<TxOption>,
}

impl Wallet {
    /// `pwallet.New(client, builder, signer)` — the builder/signer are derived
    /// from the keychain + backend on demand.
    #[must_use]
    pub fn new(
        client: Arc<dyn PChainClient>,
        backend: Arc<Backend>,
        keychain: Arc<Keychain>,
        context: Context,
    ) -> Self {
        Self {
            client,
            backend,
            keychain,
            context,
            default_options: Vec::new(),
        }
    }

    /// `pwallet.WithOptions` — a wallet that applies `options` before the
    /// per-call options on every operation.
    #[must_use]
    pub fn with_options(mut self, options: Vec<TxOption>) -> Self {
        self.default_options = union_options(&self.default_options, &options);
        self
    }

    /// The wallet's mutable backend (shared UTXO store + owners).
    #[must_use]
    pub fn backend(&self) -> &Backend {
        &self.backend
    }

    /// `Wallet.Builder()`.
    #[must_use]
    pub fn builder(&self) -> Builder<'_> {
        Builder::new(
            self.keychain.addresses(),
            self.context,
            self.backend.as_ref(),
        )
    }

    /// `Wallet.Signer()`.
    #[must_use]
    pub fn signer(&self) -> Signer<'_> {
        Signer::new(&self.keychain, self.backend.as_ref())
    }

    fn merged(&self, options: &[TxOption]) -> Vec<TxOption> {
        union_options(&self.default_options, options)
    }

    /// `IssueBaseTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_base_tx(
        &self,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_base_tx(outputs, &options)?;
        self.issue_unsigned(UnsignedTx::Base(utx), &options).await
    }

    /// `IssueAddSubnetValidatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_add_subnet_validator_tx(
        &self,
        vdr: SubnetValidator,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_add_subnet_validator_tx(vdr, &options)?;
        self.issue_unsigned(UnsignedTx::AddSubnetValidator(utx), &options)
            .await
    }

    /// `IssueRemoveSubnetValidatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_remove_subnet_validator_tx(
        &self,
        node_id: NodeId,
        subnet_id: Id,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_remove_subnet_validator_tx(node_id, subnet_id, &options)?;
        self.issue_unsigned(UnsignedTx::RemoveSubnetValidator(utx), &options)
            .await
    }

    /// `IssueCreateChainTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_create_chain_tx(
        &self,
        subnet_id: Id,
        genesis: Vec<u8>,
        vm_id: Id,
        fx_ids: Vec<Id>,
        chain_name: String,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_create_chain_tx(subnet_id, genesis, vm_id, fx_ids, chain_name, &options)?;
        self.issue_unsigned(UnsignedTx::CreateChain(utx), &options)
            .await
    }

    /// `IssueCreateSubnetTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_create_subnet_tx(
        &self,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_create_subnet_tx(owner, &options)?;
        self.issue_unsigned(UnsignedTx::CreateSubnet(utx), &options)
            .await
    }

    /// `IssueTransferSubnetOwnershipTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_transfer_subnet_ownership_tx(
        &self,
        subnet_id: Id,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_transfer_subnet_ownership_tx(subnet_id, owner, &options)?;
        self.issue_unsigned(UnsignedTx::TransferSubnetOwnership(utx), &options)
            .await
    }

    /// `IssueTransformSubnetTx` (pre-Etna legacy).
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue_transform_subnet_tx(
        &self,
        subnet_id: Id,
        asset_id: Id,
        initial_supply: u64,
        max_supply: u64,
        min_consumption_rate: u64,
        max_consumption_rate: u64,
        min_validator_stake: u64,
        max_validator_stake: u64,
        min_stake_duration_secs: u32,
        max_stake_duration_secs: u32,
        min_delegation_fee: u32,
        min_delegator_stake: u64,
        max_validator_weight_factor: u8,
        uptime_requirement: u32,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_transform_subnet_tx(
            subnet_id,
            asset_id,
            initial_supply,
            max_supply,
            min_consumption_rate,
            max_consumption_rate,
            min_validator_stake,
            max_validator_stake,
            min_stake_duration_secs,
            max_stake_duration_secs,
            min_delegation_fee,
            min_delegator_stake,
            max_validator_weight_factor,
            uptime_requirement,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::TransformSubnet(utx), &options)
            .await
    }

    /// `IssueConvertSubnetToL1Tx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_convert_subnet_to_l1_tx(
        &self,
        subnet_id: Id,
        chain_id: Id,
        address: Vec<u8>,
        validators: Vec<ConvertSubnetToL1Validator>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_convert_subnet_to_l1_tx(subnet_id, chain_id, address, validators, &options)?;
        self.issue_unsigned(UnsignedTx::ConvertSubnetToL1(utx), &options)
            .await
    }

    /// `IssueRegisterL1ValidatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_register_l1_validator_tx(
        &self,
        balance: u64,
        proof_of_possession: [u8; 96],
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_register_l1_validator_tx(
            balance,
            proof_of_possession,
            message,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::RegisterL1Validator(utx), &options)
            .await
    }

    /// `IssueSetL1ValidatorWeightTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_set_l1_validator_weight_tx(
        &self,
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_set_l1_validator_weight_tx(message, &options)?;
        self.issue_unsigned(UnsignedTx::SetL1ValidatorWeight(utx), &options)
            .await
    }

    /// `IssueIncreaseL1ValidatorBalanceTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_increase_l1_validator_balance_tx(
        &self,
        validation_id: Id,
        balance: u64,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_increase_l1_validator_balance_tx(
            validation_id,
            balance,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::IncreaseL1ValidatorBalance(utx), &options)
            .await
    }

    /// `IssueDisableL1ValidatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_disable_l1_validator_tx(
        &self,
        validation_id: Id,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_disable_l1_validator_tx(validation_id, &options)?;
        self.issue_unsigned(UnsignedTx::DisableL1Validator(utx), &options)
            .await
    }

    /// `IssueImportTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_import_tx(source_chain_id, to, &options)?;
        self.issue_unsigned(UnsignedTx::Import(utx), &options).await
    }

    /// `IssueExportTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self
            .builder()
            .new_export_tx(destination_chain_id, outputs, &options)?;
        self.issue_unsigned(UnsignedTx::Export(utx), &options).await
    }

    /// `IssueAddPermissionlessValidatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue_add_permissionless_validator_tx(
        &self,
        vdr: SubnetValidator,
        signer: PopSigner,
        asset_id: Id,
        validation_rewards_owner: OutputOwners,
        delegation_rewards_owner: OutputOwners,
        shares: u32,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_add_permissionless_validator_tx(
            vdr,
            signer,
            asset_id,
            validation_rewards_owner,
            delegation_rewards_owner,
            shares,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::AddPermissionlessValidator(utx), &options)
            .await
    }

    /// `IssueAddPermissionlessDelegatorTx`.
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_add_permissionless_delegator_tx(
        &self,
        vdr: SubnetValidator,
        asset_id: Id,
        rewards_owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_add_permissionless_delegator_tx(
            vdr,
            asset_id,
            rewards_owner,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::AddPermissionlessDelegator(utx), &options)
            .await
    }

    /// `IssueAddAutoRenewedValidatorTx` (ACP-236).
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn issue_add_auto_renewed_validator_tx(
        &self,
        validator_node_id: NodeId,
        weight: u64,
        signer: PopSigner,
        validation_rewards_owner: OutputOwners,
        delegation_rewards_owner: OutputOwners,
        validator_authority: OutputOwners,
        delegation_shares: u32,
        auto_compound_reward_shares: u32,
        period_secs: u64,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_add_auto_renewed_validator_tx(
            validator_node_id,
            weight,
            signer,
            validation_rewards_owner,
            delegation_rewards_owner,
            validator_authority,
            delegation_shares,
            auto_compound_reward_shares,
            period_secs,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::AddAutoRenewedValidator(utx), &options)
            .await
    }

    /// `IssueSetAutoRenewedValidatorConfigTx` (ACP-236).
    ///
    /// # Errors
    /// Build/sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_set_auto_renewed_validator_config_tx(
        &self,
        tx_id: Id,
        auto_compound_reward_shares: u32,
        period_secs: u64,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        let utx = self.builder().new_set_auto_renewed_validator_config_tx(
            tx_id,
            auto_compound_reward_shares,
            period_secs,
            &options,
        )?;
        self.issue_unsigned(UnsignedTx::SetAutoRenewedValidatorConfig(utx), &options)
            .await
    }

    /// `IssueUnsignedTx` — sign then issue.
    ///
    /// # Errors
    /// Sign failures; [`Error::Client`] on submit/poll failure.
    pub async fn issue_unsigned_tx(
        &self,
        unsigned: UnsignedTx,
        options: &[TxOption],
    ) -> Result<SignedTx> {
        let options = self.merged(options);
        self.issue_unsigned(unsigned, &options).await
    }

    /// `IssueTx` — submit the signed tx, await acceptance (unless
    /// [`TxOption::AssumeDecided`]) and record it in the backend.
    ///
    /// # Errors
    /// [`Error::Client`] on submit/poll failure; backend recording failures.
    pub async fn issue_tx(&self, tx: &SignedTx, options: &[TxOption]) -> Result<()> {
        let options = self.merged(options);
        self.issue_signed(tx, &options).await
    }

    /// The shared issue tail over already-merged options.
    async fn issue_unsigned(
        &self,
        unsigned: UnsignedTx,
        merged_options: &[TxOption],
    ) -> Result<SignedTx> {
        let tx = self.signer().sign_unsigned(unsigned)?;
        self.issue_signed(&tx, merged_options).await?;
        Ok(tx)
    }

    async fn issue_signed(&self, tx: &SignedTx, merged_options: &[TxOption]) -> Result<()> {
        let ops = Options::new(merged_options);
        let tx_id = self.client.issue_tx(&tx.signed_bytes).await?;
        if !ops.assume_decided() {
            self.client.await_tx_accepted(tx_id).await?;
        }
        self.backend.accept_tx(tx)
    }
}
