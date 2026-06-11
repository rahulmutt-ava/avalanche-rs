// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-chain tx builder — port of `wallet/chain/p/builder/builder.go`.
//!
//! Every method selects UTXOs **deterministically** (canonical UTXOID order,
//! locked-then-unlocked for stake, AVAX last for fees) and prices the tx
//! incrementally with the ACP-103 complexity table ([`super::fee`]), exactly
//! mirroring the Go `spendHelper`, so the produced unsigned txs are
//! byte-identical to the Go wallet's (specs 12 §13 / §12.5).

use std::collections::{BTreeMap, BTreeSet};

use ava_platformvm::signer::Signer as PopSigner;
use ava_platformvm::stakeable::{LockIn, LockOut};
use ava_platformvm::txs::components::{
    Auth, BaseTx as AvaxBaseTx, Input as FxInput, Output as FxOutput, Owner, TransferableInput,
    TransferableOutput, sort_transferable_outputs,
};
use ava_platformvm::txs::fee::gas::Dimensions;
use ava_platformvm::txs::{
    AddAutoRenewedValidatorTx, AddPermissionlessDelegatorTx, AddPermissionlessValidatorTx,
    AddSubnetValidatorTx, BaseTx, ConvertSubnetToL1Tx, ConvertSubnetToL1Validator, CreateChainTx,
    CreateSubnetTx, DisableL1ValidatorTx, ExportTx, ImportTx, IncreaseL1ValidatorBalanceTx,
    RegisterL1ValidatorTx, RemoveSubnetValidatorTx, SetAutoRenewedValidatorConfigTx,
    SetL1ValidatorWeightTx, SubnetValidator, TransferSubnetOwnershipTx, TransformSubnetTx,
};
use ava_platformvm::utxo::Utxo;
use ava_secp256k1fx::{Input as SecpInput, OutputOwners, TransferInput, TransferOutput};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_types::short_id::ShortId;

use super::backend::Backend;
use super::fee;
use super::{Context, PLATFORM_CHAIN_ID};
use crate::common::match_owners;
use crate::common::utxo_select::{sort_utxos, split_by_asset_id, split_by_locktime, unwrap_output};
use crate::error::{Error, Result};
use crate::options::{Options, TxOption};

/// The Go `builder.Builder` interface (specs 12 §13). All methods are pure
/// over the [`Backend`] snapshot.
#[allow(clippy::too_many_arguments)]
pub trait PBuilder {
    /// The chain configuration used to price/stamp txs.
    fn context(&self) -> &Context;

    /// `GetBalance` — the spendable amount of each asset on the P-chain.
    ///
    /// # Errors
    /// [`Error::UnknownOutputType`] / [`Error::Overflow`].
    fn get_balance(&self, options: &[TxOption]) -> Result<BTreeMap<Id, u64>>;

    /// `GetImportableBalance` — the importable amount of each asset exported
    /// from `source_chain_id`.
    ///
    /// # Errors
    /// [`Error::UnknownOutputType`] / [`Error::Overflow`].
    fn get_importable_balance(
        &self,
        source_chain_id: Id,
        options: &[TxOption],
    ) -> Result<BTreeMap<Id, u64>>;

    /// `NewBaseTx` — a simple value transfer.
    ///
    /// # Errors
    /// Selection/fee failures ([`Error::InsufficientFunds`], …).
    fn new_base_tx(&self, outputs: Vec<TransferableOutput>, options: &[TxOption])
    -> Result<BaseTx>;

    /// `NewAddSubnetValidatorTx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_add_subnet_validator_tx(
        &self,
        vdr: SubnetValidator,
        options: &[TxOption],
    ) -> Result<AddSubnetValidatorTx>;

    /// `NewRemoveSubnetValidatorTx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_remove_subnet_validator_tx(
        &self,
        node_id: NodeId,
        subnet_id: Id,
        options: &[TxOption],
    ) -> Result<RemoveSubnetValidatorTx>;

    /// `NewCreateChainTx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_create_chain_tx(
        &self,
        subnet_id: Id,
        genesis: Vec<u8>,
        vm_id: Id,
        fx_ids: Vec<Id>,
        chain_name: String,
        options: &[TxOption],
    ) -> Result<CreateChainTx>;

    /// `NewCreateSubnetTx`.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_create_subnet_tx(
        &self,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<CreateSubnetTx>;

    /// `NewTransferSubnetOwnershipTx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_transfer_subnet_ownership_tx(
        &self,
        subnet_id: Id,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<TransferSubnetOwnershipTx>;

    /// `NewTransformSubnetTx` (pre-Etna legacy; priced with empty intrinsic
    /// dimensions exactly like Go).
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_transform_subnet_tx(
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
    ) -> Result<TransformSubnetTx>;

    /// `NewConvertSubnetToL1Tx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_convert_subnet_to_l1_tx(
        &self,
        subnet_id: Id,
        chain_id: Id,
        address: Vec<u8>,
        validators: Vec<ConvertSubnetToL1Validator>,
        options: &[TxOption],
    ) -> Result<ConvertSubnetToL1Tx>;

    /// `NewRegisterL1ValidatorTx`.
    ///
    /// # Errors
    /// Selection/fee failures; [`Error::Codec`] on an unparsable warp message.
    fn new_register_l1_validator_tx(
        &self,
        balance: u64,
        proof_of_possession: [u8; 96],
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<RegisterL1ValidatorTx>;

    /// `NewSetL1ValidatorWeightTx`.
    ///
    /// # Errors
    /// Selection/fee failures; [`Error::Codec`] on an unparsable warp message.
    fn new_set_l1_validator_weight_tx(
        &self,
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<SetL1ValidatorWeightTx>;

    /// `NewIncreaseL1ValidatorBalanceTx`.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_increase_l1_validator_balance_tx(
        &self,
        validation_id: Id,
        balance: u64,
        options: &[TxOption],
    ) -> Result<IncreaseL1ValidatorBalanceTx>;

    /// `NewDisableL1ValidatorTx`.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_disable_l1_validator_tx(
        &self,
        validation_id: Id,
        options: &[TxOption],
    ) -> Result<DisableL1ValidatorTx>;

    /// `NewImportTx` — consume every importable UTXO from `source_chain_id`.
    ///
    /// # Errors
    /// [`Error::NoImportableFunds`] if nothing can be imported; selection/fee
    /// failures.
    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<ImportTx>;

    /// `NewExportTx`.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<ExportTx>;

    /// `NewAddPermissionlessValidatorTx`.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_add_permissionless_validator_tx(
        &self,
        vdr: SubnetValidator,
        signer: PopSigner,
        asset_id: Id,
        validation_rewards_owner: OutputOwners,
        delegation_rewards_owner: OutputOwners,
        shares: u32,
        options: &[TxOption],
    ) -> Result<AddPermissionlessValidatorTx>;

    /// `NewAddPermissionlessDelegatorTx`.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_add_permissionless_delegator_tx(
        &self,
        vdr: SubnetValidator,
        asset_id: Id,
        rewards_owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<AddPermissionlessDelegatorTx>;

    /// `NewAddAutoRenewedValidatorTx` (ACP-236 upstream delta). `period_secs`
    /// is Go's `period time.Duration` divided to whole seconds.
    ///
    /// # Errors
    /// Selection/fee failures.
    fn new_add_auto_renewed_validator_tx(
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
    ) -> Result<AddAutoRenewedValidatorTx>;

    /// `NewSetAutoRenewedValidatorConfigTx` (ACP-236 upstream delta).
    /// `period_secs == 0` triggers a graceful exit at the end of the current
    /// cycle.
    ///
    /// # Errors
    /// Selection/fee/authorization failures.
    fn new_set_auto_renewed_validator_config_tx(
        &self,
        tx_id: Id,
        auto_compound_reward_shares: u32,
        period_secs: u64,
        options: &[TxOption],
    ) -> Result<SetAutoRenewedValidatorConfigTx>;

    /// `Builder.utxos` — the canonical-order UTXO snapshot for `source_chain`.
    fn utxos(&self, source_chain: Id) -> Vec<Utxo>;

    /// `Builder.GetOwner` (via the backend).
    ///
    /// # Errors
    /// [`Error::MissingOwner`] if the owner is unknown.
    fn get_owner(&self, owner_id: Id) -> Result<OutputOwners>;
}

/// The concrete builder over a [`Backend`] snapshot (Go `builder.New`).
pub struct Builder<'a> {
    addrs: BTreeSet<ShortId>,
    context: Context,
    backend: &'a dyn Backend,
}

impl<'a> Builder<'a> {
    /// Go `builder.New(addrs, context, backend)`.
    #[must_use]
    pub fn new(addrs: BTreeSet<ShortId>, context: Context, backend: &'a dyn Backend) -> Self {
        Self {
            addrs,
            context,
            backend,
        }
    }

    fn get_balance_for(&self, chain_id: Id, ops: &Options) -> Result<BTreeMap<Id, u64>> {
        let utxos = self.backend.utxos(chain_id);
        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();
        let mut balance = BTreeMap::new();

        for utxo in &utxos {
            let (out, locktime) = unwrap_output(&utxo.out)?;
            if locktime > min_issuance_time && !ops.allow_stakeable_locked() {
                // Currently locked; cannot be burned.
                continue;
            }
            if match_owners(&out.owners, &addrs, min_issuance_time).is_none() {
                continue;
            }
            let entry = balance.entry(utxo.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amt).ok_or(Error::Overflow)?;
        }
        Ok(balance)
    }

    /// `builder.authorize` — resolve `owner_id` and match the keychain
    /// addresses against the owner threshold.
    fn authorize(&self, owner_id: Id, ops: &Options) -> Result<SecpInput> {
        let owner = self
            .backend
            .get_owner(owner_id)
            .ok_or(Error::MissingOwner(owner_id))?;
        let addrs = ops.addresses(&self.addrs);
        let sig_indices = match_owners(&owner, &addrs, ops.min_issuance_time())
            .ok_or(Error::InsufficientAuthorization)?;
        Ok(SecpInput::new(sig_indices))
    }

    /// `builder.spend` — the deterministic UTXO selection + incremental fee
    /// loop. See the Go doc comment for the exact semantics; ported 1:1.
    #[allow(clippy::too_many_lines)]
    fn spend(
        &self,
        mut to_burn: BTreeMap<Id, u64>,
        mut to_stake: BTreeMap<Id, u64>,
        mut excess_avax: u64,
        complexity: Dimensions,
        owner_override: Option<OutputOwners>,
        ops: &Options,
    ) -> Result<(
        Vec<TransferableInput>,
        Vec<TransferableOutput>,
        Vec<TransferableOutput>,
    )> {
        let mut utxos = self.backend.utxos(PLATFORM_CHAIN_ID);
        sort_utxos(&mut utxos);

        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();

        let first_addr = addrs.iter().next().ok_or(Error::NoChangeAddress)?;
        let change_owner = ops.change_owner(OutputOwners::new(0, 1, vec![*first_addr]));
        let mut owner_override = owner_override.unwrap_or_else(|| change_owner.clone());

        let mut s = SpendHelper {
            weights: self.context.complexity_weights,
            gas_price: self.context.gas_price,
            complexity,
            inputs: Vec::new(),
            change_outputs: Vec::new(),
            stake_outputs: Vec::new(),
        };

        let (unlocked, locked) = split_by_locktime(utxos, min_issuance_time);

        // 1. Locked UTXOs go toward the stake amounts first.
        for utxo in &locked {
            let asset_id = utxo.asset_id;
            if to_stake.get(&asset_id).copied().unwrap_or_default() == 0 {
                continue;
            }
            let (out, locktime) = unwrap_output(&utxo.out)?;
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };

            s.add_input(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id,
                r#in: FxInput::StakeableLock(LockIn::new(
                    locktime,
                    FxInput::Transfer(TransferInput::new(out.amt, sig_indices)),
                )),
            })?;

            let excess = consume_stake(&mut to_stake, asset_id, out.amt);
            let staked = out.amt.checked_sub(excess).ok_or(Error::Overflow)?;
            s.add_stake_output(TransferableOutput {
                asset_id,
                out: FxOutput::StakeableLock(LockOut::new(
                    locktime,
                    FxOutput::Transfer(TransferOutput::new(staked, out.owners.clone())),
                )),
            })?;

            if excess == 0 {
                continue;
            }
            s.add_change_output(TransferableOutput {
                asset_id,
                out: FxOutput::StakeableLock(LockOut::new(
                    locktime,
                    FxOutput::Transfer(TransferOutput::new(excess, out.owners.clone())),
                )),
            })?;
        }

        // 2. Remaining stake amounts are assumed to come from unlocked UTXOs:
        //    one (merged) stake output per asset, owned by the change owner.
        for (&asset_id, &amount) in &to_stake {
            if amount == 0 {
                continue;
            }
            s.add_stake_output(TransferableOutput {
                asset_id,
                out: FxOutput::Transfer(TransferOutput::new(amount, change_owner.clone())),
            })?;
        }

        // 3. Non-AVAX unlocked UTXOs (AVAX is last, to account for fees).
        let (avax_utxos, other_utxos) = split_by_asset_id(unlocked, self.context.avax_asset_id);
        for utxo in &other_utxos {
            let asset_id = utxo.asset_id;
            if !should_consume_asset(&to_burn, &to_stake, asset_id) {
                continue;
            }
            let (out, _) = unwrap_output(&utxo.out)?;
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };

            s.add_input(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id,
                r#in: FxInput::Transfer(TransferInput::new(out.amt, sig_indices)),
            })?;

            let excess = consume_asset(&mut to_burn, &mut to_stake, asset_id, out.amt);
            if excess == 0 {
                continue;
            }
            s.add_change_output(TransferableOutput {
                asset_id,
                out: FxOutput::Transfer(TransferOutput::new(excess, change_owner.clone())),
            })?;
        }

        // 4. AVAX UTXOs, stopping as soon as the accrued fee is covered.
        let avax_asset_id = self.context.avax_asset_id;
        for utxo in &avax_utxos {
            let required_fee = s.calculate_fee()?;
            if !should_consume_asset(&to_burn, &to_stake, avax_asset_id)
                && excess_avax >= required_fee
            {
                break;
            }

            let (out, _) = unwrap_output(&utxo.out)?;
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };

            s.add_input(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id: avax_asset_id,
                r#in: FxInput::Transfer(TransferInput::new(out.amt, sig_indices)),
            })?;

            let excess = consume_asset(&mut to_burn, &mut to_stake, avax_asset_id, out.amt);
            excess_avax = excess_avax.checked_add(excess).ok_or(Error::Overflow)?;

            // Additional AVAX was consumed: change goes to the change owner.
            owner_override = change_owner.clone();
        }

        // 5. Everything requested must have been consumed.
        for (&asset_id, &amount) in to_stake.iter().chain(to_burn.iter()) {
            if amount != 0 {
                return Err(Error::InsufficientFunds { amount, asset_id });
            }
        }

        let required_fee = s.calculate_fee()?;
        if excess_avax < required_fee {
            return Err(Error::InsufficientFunds {
                amount: required_fee
                    .checked_sub(excess_avax)
                    .ok_or(Error::Overflow)?,
                asset_id: avax_asset_id,
            });
        }

        // 6. Add the AVAX change output iff it pays for its own complexity.
        let excess_output_owner = owner_override;
        let excess_output_probe = TransferableOutput {
            asset_id: avax_asset_id,
            out: FxOutput::Transfer(TransferOutput::new(0, excess_output_owner.clone())),
        };
        s.add_output_complexity(&excess_output_probe)?;
        let required_fee_with_change = s.calculate_fee()?;
        if excess_avax > required_fee_with_change {
            let amt = excess_avax
                .checked_sub(required_fee_with_change)
                .ok_or(Error::Overflow)?;
            s.change_outputs.push(TransferableOutput {
                asset_id: avax_asset_id,
                out: FxOutput::Transfer(TransferOutput::new(amt, excess_output_owner)),
            });
        }

        s.inputs.sort_by(TransferableInput::compare);
        sort_transferable_outputs(&mut s.change_outputs);
        sort_transferable_outputs(&mut s.stake_outputs);
        Ok((s.inputs, s.change_outputs, s.stake_outputs))
    }

    fn base_tx(
        &self,
        inputs: Vec<TransferableInput>,
        outputs: Vec<TransferableOutput>,
        memo: &[u8],
    ) -> AvaxBaseTx {
        AvaxBaseTx {
            network_id: self.context.network_id,
            blockchain_id: PLATFORM_CHAIN_ID,
            outs: outputs,
            ins: inputs,
            memo: memo.to_vec(),
        }
    }
}

/// `spendHelper.shouldConsumeAsset`.
fn should_consume_asset(
    to_burn: &BTreeMap<Id, u64>,
    to_stake: &BTreeMap<Id, u64>,
    asset_id: Id,
) -> bool {
    to_burn.get(&asset_id).copied().unwrap_or_default() != 0
        || to_stake.get(&asset_id).copied().unwrap_or_default() != 0
}

/// `spendHelper.consumeLockedAsset` — stake as much of `amount` as still
/// needed; returns the excess.
fn consume_stake(to_stake: &mut BTreeMap<Id, u64>, asset_id: Id, amount: u64) -> u64 {
    let entry = to_stake.entry(asset_id).or_insert(0);
    let staked = (*entry).min(amount);
    *entry = entry.saturating_sub(staked);
    amount.saturating_sub(staked)
}

/// `spendHelper.consumeAsset` — burn first, stake the rest; returns the
/// excess.
fn consume_asset(
    to_burn: &mut BTreeMap<Id, u64>,
    to_stake: &mut BTreeMap<Id, u64>,
    asset_id: Id,
    amount: u64,
) -> u64 {
    let entry = to_burn.entry(asset_id).or_insert(0);
    let burned = (*entry).min(amount);
    *entry = entry.saturating_sub(burned);
    consume_stake(to_stake, asset_id, amount.saturating_sub(burned))
}

struct SpendHelper {
    weights: Dimensions,
    gas_price: u64,
    complexity: Dimensions,
    inputs: Vec<TransferableInput>,
    change_outputs: Vec<TransferableOutput>,
    stake_outputs: Vec<TransferableOutput>,
}

impl SpendHelper {
    fn add_input(&mut self, input: TransferableInput) -> Result<()> {
        let c = fee::input_complexity(std::slice::from_ref(&input))?;
        self.complexity = fee::add(self.complexity, &[c])?;
        self.inputs.push(input);
        Ok(())
    }

    fn add_change_output(&mut self, output: TransferableOutput) -> Result<()> {
        self.add_output_complexity(&output)?;
        self.change_outputs.push(output);
        Ok(())
    }

    fn add_stake_output(&mut self, output: TransferableOutput) -> Result<()> {
        self.add_output_complexity(&output)?;
        self.stake_outputs.push(output);
        Ok(())
    }

    fn add_output_complexity(&mut self, output: &TransferableOutput) -> Result<()> {
        let c = fee::output_complexity(std::slice::from_ref(output))?;
        self.complexity = fee::add(self.complexity, &[c])?;
        Ok(())
    }

    fn calculate_fee(&self) -> Result<u64> {
        fee::calculate_fee(self.complexity, self.weights, self.gas_price)
    }
}

fn sorted(mut owner: OutputOwners) -> OutputOwners {
    owner.addrs.sort();
    owner
}

impl PBuilder for Builder<'_> {
    fn context(&self) -> &Context {
        &self.context
    }

    fn get_balance(&self, options: &[TxOption]) -> Result<BTreeMap<Id, u64>> {
        let ops = Options::new(options);
        self.get_balance_for(PLATFORM_CHAIN_ID, &ops)
    }

    fn get_importable_balance(
        &self,
        source_chain_id: Id,
        options: &[TxOption],
    ) -> Result<BTreeMap<Id, u64>> {
        let ops = Options::new(options);
        self.get_balance_for(source_chain_id, &ops)
    }

    fn new_base_tx(
        &self,
        mut outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<BaseTx> {
        let mut to_burn = BTreeMap::new();
        for out in &outputs {
            let entry = to_burn.entry(out.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amount()).ok_or(Error::Overflow)?;
        }

        let ops = Options::new(options);
        let complexity = fee::add(
            fee::INTRINSIC_BASE_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::output_complexity(&outputs)?,
            ],
        )?;

        let (inputs, change_outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, complexity, None, &ops)?;
        outputs.extend(change_outputs);
        sort_transferable_outputs(&mut outputs);

        Ok(BaseTx::new(self.base_tx(inputs, outputs, ops.memo())))
    }

    fn new_add_subnet_validator_tx(
        &self,
        vdr: SubnetValidator,
        options: &[TxOption],
    ) -> Result<AddSubnetValidatorTx> {
        let ops = Options::new(options);
        let subnet_auth = self.authorize(vdr.subnet, &ops)?;

        let complexity = fee::add(
            fee::INTRINSIC_ADD_SUBNET_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::auth_complexity(&subnet_auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(AddSubnetValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            subnet_validator: vdr,
            subnet_auth: Auth::Secp256k1(subnet_auth),
        })
    }

    fn new_remove_subnet_validator_tx(
        &self,
        node_id: NodeId,
        subnet_id: Id,
        options: &[TxOption],
    ) -> Result<RemoveSubnetValidatorTx> {
        let ops = Options::new(options);
        let subnet_auth = self.authorize(subnet_id, &ops)?;

        let complexity = fee::add(
            fee::INTRINSIC_REMOVE_SUBNET_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::auth_complexity(&subnet_auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(RemoveSubnetValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            node_id,
            subnet: subnet_id,
            subnet_auth: Auth::Secp256k1(subnet_auth),
        })
    }

    fn new_create_chain_tx(
        &self,
        subnet_id: Id,
        genesis: Vec<u8>,
        vm_id: Id,
        mut fx_ids: Vec<Id>,
        chain_name: String,
        options: &[TxOption],
    ) -> Result<CreateChainTx> {
        let ops = Options::new(options);
        let subnet_auth = self.authorize(subnet_id, &ops)?;

        let dynamic_bytes = fx_ids
            .len()
            .checked_mul(32)
            .and_then(|n| n.checked_add(chain_name.len()))
            .and_then(|n| n.checked_add(genesis.len()))
            .and_then(|n| n.checked_add(ops.memo().len()))
            .ok_or(Error::Overflow)?;
        let complexity = fee::add(
            fee::INTRINSIC_CREATE_CHAIN_TX,
            &[
                fee::bandwidth(dynamic_bytes),
                fee::auth_complexity(&subnet_auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        fx_ids.sort_by_key(|id| id.to_bytes());
        Ok(CreateChainTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            subnet_id,
            chain_name,
            vm_id,
            fx_ids,
            genesis_data: genesis,
            subnet_auth: Auth::Secp256k1(subnet_auth),
        })
    }

    fn new_create_subnet_tx(
        &self,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<CreateSubnetTx> {
        let ops = Options::new(options);
        let complexity = fee::add(
            fee::INTRINSIC_CREATE_SUBNET_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::owner_complexity(&owner)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(CreateSubnetTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            owner: Owner::Secp256k1(sorted(owner)),
        })
    }

    fn new_transfer_subnet_ownership_tx(
        &self,
        subnet_id: Id,
        owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<TransferSubnetOwnershipTx> {
        let ops = Options::new(options);
        let subnet_auth = self.authorize(subnet_id, &ops)?;

        let complexity = fee::add(
            fee::INTRINSIC_TRANSFER_SUBNET_OWNERSHIP_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::auth_complexity(&subnet_auth)?,
                fee::owner_complexity(&owner)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(TransferSubnetOwnershipTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            subnet: subnet_id,
            subnet_auth: Auth::Secp256k1(subnet_auth),
            owner: Owner::Secp256k1(sorted(owner)),
        })
    }

    fn new_transform_subnet_tx(
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
    ) -> Result<TransformSubnetTx> {
        let ops = Options::new(options);
        let subnet_auth = self.authorize(subnet_id, &ops)?;

        let to_burn = BTreeMap::from([(
            asset_id,
            max_supply
                .checked_sub(initial_supply)
                .ok_or(Error::Overflow)?,
        )]);
        let (inputs, outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, [0, 0, 0, 0], None, &ops)?;

        Ok(TransformSubnetTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            subnet: subnet_id,
            asset_id,
            initial_supply,
            maximum_supply: max_supply,
            min_consumption_rate,
            max_consumption_rate,
            min_validator_stake,
            max_validator_stake,
            min_stake_duration: min_stake_duration_secs,
            max_stake_duration: max_stake_duration_secs,
            min_delegation_fee,
            min_delegator_stake,
            max_validator_weight_factor,
            uptime_requirement,
            subnet_auth: Auth::Secp256k1(subnet_auth),
        })
    }

    fn new_convert_subnet_to_l1_tx(
        &self,
        subnet_id: Id,
        chain_id: Id,
        address: Vec<u8>,
        mut validators: Vec<ConvertSubnetToL1Validator>,
        options: &[TxOption],
    ) -> Result<ConvertSubnetToL1Tx> {
        let mut avax_to_burn = 0u64;
        for vdr in &validators {
            avax_to_burn = avax_to_burn
                .checked_add(vdr.balance)
                .ok_or(Error::Overflow)?;
        }
        let to_burn = BTreeMap::from([(self.context.avax_asset_id, avax_to_burn)]);

        let ops = Options::new(options);
        let subnet_auth = self.authorize(subnet_id, &ops)?;

        let additional_bytes = ops
            .memo()
            .len()
            .checked_add(address.len())
            .ok_or(Error::Overflow)?;
        let complexity = fee::add(
            fee::INTRINSIC_CONVERT_SUBNET_TO_L1_TX,
            &[
                fee::bandwidth(additional_bytes),
                fee::convert_subnet_to_l1_validator_complexity(&validators)?,
                fee::auth_complexity(&subnet_auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, complexity, None, &ops)?;

        validators.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        Ok(ConvertSubnetToL1Tx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            subnet: subnet_id,
            chain_id,
            address,
            validators,
            subnet_auth: Auth::Secp256k1(subnet_auth),
        })
    }

    fn new_register_l1_validator_tx(
        &self,
        balance: u64,
        proof_of_possession: [u8; 96],
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<RegisterL1ValidatorTx> {
        let to_burn = BTreeMap::from([(self.context.avax_asset_id, balance)]);
        let ops = Options::new(options);

        let complexity = fee::add(
            fee::INTRINSIC_REGISTER_L1_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::warp_complexity(&message)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(RegisterL1ValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            balance,
            proof_of_possession,
            message,
        })
    }

    fn new_set_l1_validator_weight_tx(
        &self,
        message: Vec<u8>,
        options: &[TxOption],
    ) -> Result<SetL1ValidatorWeightTx> {
        let ops = Options::new(options);
        let complexity = fee::add(
            fee::INTRINSIC_SET_L1_VALIDATOR_WEIGHT_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::warp_complexity(&message)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(SetL1ValidatorWeightTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            message,
        })
    }

    fn new_increase_l1_validator_balance_tx(
        &self,
        validation_id: Id,
        balance: u64,
        options: &[TxOption],
    ) -> Result<IncreaseL1ValidatorBalanceTx> {
        let to_burn = BTreeMap::from([(self.context.avax_asset_id, balance)]);
        let ops = Options::new(options);

        let complexity = fee::add(
            fee::INTRINSIC_INCREASE_L1_VALIDATOR_BALANCE_TX,
            &[fee::bandwidth(ops.memo().len())],
        )?;

        let (inputs, outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(IncreaseL1ValidatorBalanceTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            validation_id,
            balance,
        })
    }

    fn new_disable_l1_validator_tx(
        &self,
        validation_id: Id,
        options: &[TxOption],
    ) -> Result<DisableL1ValidatorTx> {
        let ops = Options::new(options);
        let disable_auth = self.authorize(validation_id, &ops)?;

        let complexity = fee::add(
            fee::INTRINSIC_DISABLE_L1_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::auth_complexity(&disable_auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(DisableL1ValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            validation_id,
            disable_auth: Auth::Secp256k1(disable_auth),
        })
    }

    fn new_import_tx(
        &self,
        source_chain_id: Id,
        to: OutputOwners,
        options: &[TxOption],
    ) -> Result<ImportTx> {
        let ops = Options::new(options);
        let mut utxos = self.backend.utxos(source_chain_id);
        sort_utxos(&mut utxos);

        let addrs = ops.addresses(&self.addrs);
        let min_issuance_time = ops.min_issuance_time();
        let avax_asset_id = self.context.avax_asset_id;

        let mut imported_inputs = Vec::with_capacity(utxos.len());
        let mut imported_amounts: BTreeMap<Id, u64> = BTreeMap::new();
        for utxo in &utxos {
            // Only plain transfer outputs are importable.
            let FxOutput::Transfer(out) = &utxo.out else {
                continue;
            };
            let Some(sig_indices) = match_owners(&out.owners, &addrs, min_issuance_time) else {
                continue;
            };
            imported_inputs.push(TransferableInput {
                tx_id: utxo.tx_id,
                output_index: utxo.output_index,
                asset_id: utxo.asset_id,
                r#in: FxInput::Transfer(TransferInput::new(out.amt, sig_indices)),
            });
            let entry = imported_amounts.entry(utxo.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amt).ok_or(Error::Overflow)?;
        }
        imported_inputs.sort_by(TransferableInput::compare);

        if imported_inputs.is_empty() {
            return Err(Error::NoImportableFunds);
        }

        let mut outputs = Vec::with_capacity(imported_amounts.len());
        for (&asset_id, &amount) in &imported_amounts {
            if asset_id == avax_asset_id {
                continue;
            }
            outputs.push(TransferableOutput {
                asset_id,
                out: FxOutput::Transfer(TransferOutput::new(amount, to.clone())),
            });
        }

        let complexity = fee::add(
            fee::INTRINSIC_IMPORT_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::input_complexity(&imported_inputs)?,
                fee::output_complexity(&outputs)?,
            ],
        )?;

        let excess_avax = imported_amounts
            .get(&avax_asset_id)
            .copied()
            .unwrap_or_default();
        let (inputs, change_outputs, _) = self.spend(
            BTreeMap::new(),
            BTreeMap::new(),
            excess_avax,
            complexity,
            Some(to),
            &ops,
        )?;
        outputs.extend(change_outputs);
        sort_transferable_outputs(&mut outputs);

        Ok(ImportTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            source_chain: source_chain_id,
            imported_inputs,
        })
    }

    fn new_export_tx(
        &self,
        destination_chain_id: Id,
        mut outputs: Vec<TransferableOutput>,
        options: &[TxOption],
    ) -> Result<ExportTx> {
        let mut to_burn = BTreeMap::new();
        for out in &outputs {
            let entry = to_burn.entry(out.asset_id).or_insert(0u64);
            *entry = entry.checked_add(out.amount()).ok_or(Error::Overflow)?;
        }

        let ops = Options::new(options);
        let complexity = fee::add(
            fee::INTRINSIC_EXPORT_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::output_complexity(&outputs)?,
            ],
        )?;

        let (inputs, change_outputs, _) =
            self.spend(to_burn, BTreeMap::new(), 0, complexity, None, &ops)?;

        sort_transferable_outputs(&mut outputs);
        Ok(ExportTx {
            base: BaseTx::new(self.base_tx(inputs, change_outputs, ops.memo())),
            destination_chain: destination_chain_id,
            exported_outputs: outputs,
        })
    }

    fn new_add_permissionless_validator_tx(
        &self,
        vdr: SubnetValidator,
        signer: PopSigner,
        asset_id: Id,
        validation_rewards_owner: OutputOwners,
        delegation_rewards_owner: OutputOwners,
        shares: u32,
        options: &[TxOption],
    ) -> Result<AddPermissionlessValidatorTx> {
        let to_stake = BTreeMap::from([(asset_id, vdr.validator.wght)]);
        let ops = Options::new(options);

        let complexity = fee::add(
            fee::INTRINSIC_ADD_PERMISSIONLESS_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::signer_complexity(&signer),
                fee::owner_complexity(&validation_rewards_owner)?,
                fee::owner_complexity(&delegation_rewards_owner)?,
            ],
        )?;

        let (inputs, base_outputs, stake_outputs) =
            self.spend(BTreeMap::new(), to_stake, 0, complexity, None, &ops)?;

        Ok(AddPermissionlessValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, base_outputs, ops.memo())),
            validator: vdr.validator,
            subnet: vdr.subnet,
            signer,
            stake_outs: stake_outputs,
            validator_rewards_owner: Owner::Secp256k1(sorted(validation_rewards_owner)),
            delegator_rewards_owner: Owner::Secp256k1(sorted(delegation_rewards_owner)),
            delegation_shares: shares,
            verified: Default::default(),
        })
    }

    fn new_add_permissionless_delegator_tx(
        &self,
        vdr: SubnetValidator,
        asset_id: Id,
        rewards_owner: OutputOwners,
        options: &[TxOption],
    ) -> Result<AddPermissionlessDelegatorTx> {
        let to_stake = BTreeMap::from([(asset_id, vdr.validator.wght)]);
        let ops = Options::new(options);

        let complexity = fee::add(
            fee::INTRINSIC_ADD_PERMISSIONLESS_DELEGATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::owner_complexity(&rewards_owner)?,
            ],
        )?;

        let (inputs, base_outputs, stake_outputs) =
            self.spend(BTreeMap::new(), to_stake, 0, complexity, None, &ops)?;

        Ok(AddPermissionlessDelegatorTx {
            base: BaseTx::new(self.base_tx(inputs, base_outputs, ops.memo())),
            validator: vdr.validator,
            subnet: vdr.subnet,
            stake_outs: stake_outputs,
            delegation_rewards_owner: Owner::Secp256k1(sorted(rewards_owner)),
        })
    }

    fn new_add_auto_renewed_validator_tx(
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
    ) -> Result<AddAutoRenewedValidatorTx> {
        let to_stake = BTreeMap::from([(self.context.avax_asset_id, weight)]);
        let ops = Options::new(options);

        let complexity = fee::add(
            fee::INTRINSIC_ADD_AUTO_RENEWED_VALIDATOR_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::signer_complexity(&signer),
                fee::owner_complexity(&validation_rewards_owner)?,
                fee::owner_complexity(&delegation_rewards_owner)?,
                fee::owner_complexity(&validator_authority)?,
            ],
        )?;

        let (inputs, base_outputs, stake_outputs) =
            self.spend(BTreeMap::new(), to_stake, 0, complexity, None, &ops)?;

        Ok(AddAutoRenewedValidatorTx {
            base: BaseTx::new(self.base_tx(inputs, base_outputs, ops.memo())),
            validator_node_id: validator_node_id.to_bytes().to_vec(),
            signer,
            stake_outs: stake_outputs,
            validator_rewards_owner: Owner::Secp256k1(sorted(validation_rewards_owner)),
            delegator_rewards_owner: Owner::Secp256k1(sorted(delegation_rewards_owner)),
            validator_authority: Owner::Secp256k1(sorted(validator_authority)),
            delegation_shares,
            auto_compound_reward_shares,
            period: period_secs,
        })
    }

    fn new_set_auto_renewed_validator_config_tx(
        &self,
        tx_id: Id,
        auto_compound_reward_shares: u32,
        period_secs: u64,
        options: &[TxOption],
    ) -> Result<SetAutoRenewedValidatorConfigTx> {
        let ops = Options::new(options);
        let auth = self.authorize(tx_id, &ops)?;

        let complexity = fee::add(
            fee::INTRINSIC_SET_AUTO_RENEWED_VALIDATOR_CONFIG_TX,
            &[
                fee::bandwidth(ops.memo().len()),
                fee::auth_complexity(&auth)?,
            ],
        )?;

        let (inputs, outputs, _) =
            self.spend(BTreeMap::new(), BTreeMap::new(), 0, complexity, None, &ops)?;

        Ok(SetAutoRenewedValidatorConfigTx {
            base: BaseTx::new(self.base_tx(inputs, outputs, ops.memo())),
            tx_id,
            auth: Auth::Secp256k1(auth),
            auto_compound_reward_shares,
            period: period_secs,
        })
    }

    fn utxos(&self, source_chain: Id) -> Vec<Utxo> {
        let mut utxos = self.backend.utxos(source_chain);
        sort_utxos(&mut utxos);
        utxos
    }

    fn get_owner(&self, owner_id: Id) -> Result<OutputOwners> {
        self.backend
            .get_owner(owner_id)
            .ok_or(Error::MissingOwner(owner_id))
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects
)]
mod tests {
    use std::collections::BTreeMap;

    use ava_platformvm::signer::ProofOfPossession;
    use ava_platformvm::txs::Validator;
    use ava_platformvm::txs::components::PChainOwner;
    use ava_types::constants::UNIT_TEST_ID;

    use super::*;
    use crate::keychain::Keychain;
    use crate::p::backend::WalletBackend;
    use crate::p::signer::Signer;

    // --- the Go-side fixture (wallet_avalanche_rs_vectors_p_test.go) ---

    const MIN_ISSUANCE_TIME: u64 = 1_700_000_000;
    const LOCK_TIME: u64 = 1_800_000_000;
    const VALIDATOR_END: u64 = 1_750_000_000;

    const NANO_AVAX: u64 = 1;
    const MILLI_AVAX: u64 = 1_000_000;
    const AVAX: u64 = 1_000_000_000;
    const MEGA_AVAX: u64 = 1_000_000_000_000_000;

    fn test_keys() -> Vec<ava_crypto::secp256k1::PrivateKey> {
        // Go `secp256k1.TestKeys()`.
        [
            "24jUJ9vZexUM6expyMcT48LBx27k1m7xpraoV62oSQAHdziao5",
            "2MMvUMsxx6zsHSNXJdFD8yc5XkancvwyKPwpw4xUK3TCGDuNBY",
            "cxb7KpGWhDMALTjNNSJ7UQkkomPesyWAPUaWRGdyeBNzR6f35",
            "ewoqjP7PxY4yr3iLTpLisriqt94hdyDFNgchSxGGztUrTXtNN",
            "2RWLv6YVEXDiWLpaCbXhhqxtLbnFaKQsWPSSMSPhpWo47uJAeV",
        ]
        .iter()
        .map(|s| {
            let b = ava_crypto::cb58::cb58_decode(s).expect("decode");
            ava_crypto::secp256k1::PrivateKey::from_bytes(&b).expect("key")
        })
        .collect()
    }

    fn avax_asset_id() -> Id {
        Id::EMPTY.prefix(&[1789])
    }

    fn subnet_asset_id() -> Id {
        Id::EMPTY.prefix(&[2024])
    }

    fn subnet_id() -> Id {
        Id::EMPTY.prefix(&[7777])
    }

    fn validation_id() -> Id {
        Id::EMPTY.prefix(&[8888])
    }

    fn other_chain_id() -> Id {
        Id::EMPTY.prefix(&[6161])
    }

    fn node_id() -> NodeId {
        NodeId::from_slice(&[
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14,
        ])
        .expect("node id")
    }

    fn short_id(b: u8) -> ShortId {
        ShortId::from_slice(&[b; 20]).expect("short id")
    }

    fn test_context() -> Context {
        Context {
            network_id: UNIT_TEST_ID,
            avax_asset_id: avax_asset_id(),
            complexity_weights: [1, 10, 100, 1000],
            gas_price: 1,
        }
    }

    fn secp_utxo(prefix: u64, asset_id: Id, amt: u64, addr: ShortId) -> Utxo {
        Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id,
            out: FxOutput::Transfer(TransferOutput::new(
                amt,
                OutputOwners::new(0, 1, vec![addr]),
            )),
        }
    }

    fn locked_utxo(prefix: u64, amt: u64, addr: ShortId) -> Utxo {
        Utxo {
            tx_id: Id::EMPTY.prefix(&[prefix]),
            output_index: u32::try_from(prefix).expect("index"),
            asset_id: avax_asset_id(),
            out: FxOutput::StakeableLock(LockOut::new(
                LOCK_TIME,
                FxOutput::Transfer(TransferOutput::new(
                    amt,
                    OutputOwners::new(0, 1, vec![addr]),
                )),
            )),
        }
    }

    /// Go `rsMakeTestUTXOs`.
    fn default_utxos(utxo_addr: ShortId) -> Vec<Utxo> {
        vec![
            secp_utxo(2024, avax_asset_id(), 2 * MILLI_AVAX, utxo_addr),
            locked_utxo(2025, 3 * MILLI_AVAX, utxo_addr),
            secp_utxo(2026, subnet_asset_id(), 99 * MEGA_AVAX, utxo_addr),
            locked_utxo(2027, 88 * AVAX, utxo_addr),
            secp_utxo(2028, avax_asset_id(), 9 * AVAX, utxo_addr),
        ]
    }

    /// Go `rsMakeUTXOs(utxoKey, 1 nAVAX, 9 AVAX)`.
    fn staker_utxos(utxo_addr: ShortId) -> Vec<Utxo> {
        vec![
            secp_utxo(2025, avax_asset_id(), NANO_AVAX, utxo_addr),
            secp_utxo(2026, avax_asset_id(), 9 * AVAX, utxo_addr),
        ]
    }

    fn bls_pop(scalar: u8) -> (ava_crypto::bls::SecretKey, ProofOfPossession) {
        let mut sk_bytes = [0u8; 32];
        sk_bytes[31] = scalar;
        let sk = ava_crypto::bls::SecretKey::from_bytes(&sk_bytes).expect("bls sk");
        let pk = sk.public_key().compress();
        let proof = sk.sign_pop(&pk).compress();
        (sk, ProofOfPossession::new(pk, proof))
    }

    struct Env {
        keychain: Keychain,
        backend: WalletBackend,
        context: Context,
        addrs: BTreeSet<ShortId>,
        utxo_owner: OutputOwners,
        subnet_owner: OutputOwners,
        validation_owner: OutputOwners,
    }

    impl Env {
        fn new(utxos: Vec<Utxo>, extra_chains: BTreeMap<Id, Vec<Utxo>>) -> Self {
            let keys = test_keys();
            let subnet_auth_addr = keys[0].public_key().address();
            let utxo_addr = keys[1].public_key().address();
            let validation_auth_addr = keys[2].public_key().address();

            let utxo_owner = OutputOwners::new(0, 1, vec![utxo_addr]);
            let subnet_owner = OutputOwners::new(0, 1, vec![subnet_auth_addr]);
            let validation_owner = OutputOwners::new(0, 1, vec![validation_auth_addr]);

            let mut utxo_sets = extra_chains;
            utxo_sets.insert(PLATFORM_CHAIN_ID, utxos);
            let owners = BTreeMap::from([
                (subnet_id(), subnet_owner.clone()),
                (validation_id(), validation_owner.clone()),
            ]);

            Self {
                keychain: Keychain::new(keys),
                backend: WalletBackend::new(utxo_sets, owners),
                context: test_context(),
                addrs: BTreeSet::from([utxo_addr, subnet_auth_addr, validation_auth_addr]),
                utxo_owner,
                subnet_owner,
                validation_owner,
            }
        }

        fn default() -> Self {
            let keys = test_keys();
            let utxo_addr = keys[1].public_key().address();
            Self::new(default_utxos(utxo_addr), BTreeMap::new())
        }

        fn builder(&self) -> Builder<'_> {
            Builder::new(self.addrs.clone(), self.context, &self.backend)
        }

        fn opts(&self) -> Vec<TxOption> {
            vec![
                TxOption::MinIssuanceTime(MIN_ISSUANCE_TIME),
                TxOption::ChangeOwner(self.utxo_owner.clone()),
            ]
        }

        /// Builds the unsigned + signed bytes and compares against the Go
        /// vector.
        fn check(&self, name: &str, unsigned: ava_platformvm::txs::UnsignedTx) {
            let vector = load_vector(name);
            let unsigned_bytes = ava_platformvm::txs::Codec()
                .marshal(ava_platformvm::CODEC_VERSION, &unsigned)
                .expect("marshal unsigned");
            assert_eq!(
                hex::encode(&unsigned_bytes),
                vector.unsigned_hex,
                "unsigned bytes mismatch for {name}"
            );

            let signer = Signer::new(&self.keychain, &self.backend);
            let signed = signer.sign_unsigned(unsigned).expect("sign");
            assert_eq!(
                hex::encode(&signed.signed_bytes),
                vector.signed_hex,
                "signed bytes mismatch for {name}"
            );
        }
    }

    #[derive(serde::Deserialize)]
    struct Vector {
        name: String,
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        unsigned_hex: String,
        signed_hex: String,
    }

    #[derive(serde::Deserialize)]
    struct VectorFile {
        vectors: Vec<Vector>,
    }

    fn load_vector(name: &str) -> Vector {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/wallet/p.json");
        let data = std::fs::read_to_string(path).expect("read vectors");
        let file: VectorFile = serde_json::from_str(&data).expect("parse vectors");
        file.vectors
            .into_iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("missing vector {name}"))
    }

    fn avax_output(env: &Env) -> TransferableOutput {
        TransferableOutput {
            asset_id: avax_asset_id(),
            out: FxOutput::Transfer(TransferOutput::new(7 * AVAX, env.utxo_owner.clone())),
        }
    }

    #[test]
    fn insufficient_funds_is_reported() {
        let env = Env::default();
        // 1000 AVAX is far beyond the fixture's spendable balance.
        let too_much = TransferableOutput {
            asset_id: avax_asset_id(),
            out: FxOutput::Transfer(TransferOutput::new(1_000 * AVAX, env.utxo_owner.clone())),
        };
        let err = env
            .builder()
            .new_base_tx(vec![too_much], &env.opts())
            .expect_err("must fail");
        assert_matches::assert_matches!(err, Error::InsufficientFunds { .. });
    }

    #[test]
    fn new_base_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_base_tx(vec![avax_output(&env)], &env.opts())
            .expect("build");
        env.check("p_base", ava_platformvm::txs::UnsignedTx::Base(tx));
    }

    #[test]
    fn new_base_tx_with_memo_bytes_match_go() {
        let env = Env::default();
        let mut opts = env.opts();
        opts.push(TxOption::Memo(b"memo".to_vec()));
        let tx = env
            .builder()
            .new_base_tx(vec![avax_output(&env)], &opts)
            .expect("build");
        env.check("p_base_memo", ava_platformvm::txs::UnsignedTx::Base(tx));
    }

    #[test]
    fn new_add_subnet_validator_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_add_subnet_validator_tx(
                SubnetValidator {
                    validator: Validator {
                        node_id: node_id(),
                        start: 0,
                        end: VALIDATOR_END,
                        wght: 0,
                    },
                    subnet: subnet_id(),
                },
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_add_subnet_validator",
            ava_platformvm::txs::UnsignedTx::AddSubnetValidator(tx),
        );
    }

    #[test]
    fn new_remove_subnet_validator_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_remove_subnet_validator_tx(node_id(), subnet_id(), &env.opts())
            .expect("build");
        env.check(
            "p_remove_subnet_validator",
            ava_platformvm::txs::UnsignedTx::RemoveSubnetValidator(tx),
        );
    }

    #[test]
    fn new_create_chain_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_create_chain_tx(
                subnet_id(),
                b"abc".to_vec(),
                Id::EMPTY.prefix(&[4242]),
                vec![Id::EMPTY.prefix(&[5151])],
                "dummyChain".to_string(),
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_create_chain",
            ava_platformvm::txs::UnsignedTx::CreateChain(tx),
        );
    }

    #[test]
    fn new_create_subnet_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_create_subnet_tx(env.subnet_owner.clone(), &env.opts())
            .expect("build");
        env.check(
            "p_create_subnet",
            ava_platformvm::txs::UnsignedTx::CreateSubnet(tx),
        );
    }

    #[test]
    fn new_transfer_subnet_ownership_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_transfer_subnet_ownership_tx(subnet_id(), env.subnet_owner.clone(), &env.opts())
            .expect("build");
        env.check(
            "p_transfer_subnet_ownership",
            ava_platformvm::txs::UnsignedTx::TransferSubnetOwnership(tx),
        );
    }

    #[test]
    fn new_import_tx_bytes_match_go() {
        let keys = test_keys();
        let utxo_addr = keys[1].public_key().address();
        let import_utxos = vec![default_utxos(utxo_addr)[0].clone()];
        let env = Env::new(
            default_utxos(utxo_addr),
            BTreeMap::from([(other_chain_id(), import_utxos)]),
        );
        let tx = env
            .builder()
            .new_import_tx(other_chain_id(), env.subnet_owner.clone(), &env.opts())
            .expect("build");
        env.check("p_import", ava_platformvm::txs::UnsignedTx::Import(tx));
    }

    #[test]
    fn new_export_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_export_tx(other_chain_id(), vec![avax_output(&env)], &env.opts())
            .expect("build");
        env.check("p_export", ava_platformvm::txs::UnsignedTx::Export(tx));
    }

    #[test]
    fn new_add_permissionless_validator_tx_bytes_match_go() {
        let keys = test_keys();
        let utxo_addr = keys[1].public_key().address();
        let env = Env::new(staker_utxos(utxo_addr), BTreeMap::new());

        let vector = load_vector("p_add_permissionless_validator");
        let (_, pop) = bls_pop(0x25);
        assert_eq!(hex::encode(pop.public_key), vector.inputs["bls_pk_0"]);
        assert_eq!(hex::encode(pop.proof), vector.inputs["bls_pop_0"]);

        let tx = env
            .builder()
            .new_add_permissionless_validator_tx(
                SubnetValidator {
                    validator: Validator {
                        node_id: node_id(),
                        start: 0,
                        end: VALIDATOR_END,
                        wght: 2 * AVAX,
                    },
                    subnet: ava_types::constants::PRIMARY_NETWORK_ID,
                },
                PopSigner::ProofOfPossession(pop),
                avax_asset_id(),
                env.subnet_owner.clone(),
                env.subnet_owner.clone(),
                1_000_000,
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_add_permissionless_validator",
            ava_platformvm::txs::UnsignedTx::AddPermissionlessValidator(tx),
        );
    }

    #[test]
    fn new_add_permissionless_delegator_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_add_permissionless_delegator_tx(
                SubnetValidator {
                    validator: Validator {
                        node_id: node_id(),
                        start: 0,
                        end: VALIDATOR_END,
                        wght: 2 * AVAX,
                    },
                    subnet: ava_types::constants::PRIMARY_NETWORK_ID,
                },
                avax_asset_id(),
                env.subnet_owner.clone(),
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_add_permissionless_delegator",
            ava_platformvm::txs::UnsignedTx::AddPermissionlessDelegator(tx),
        );
    }

    #[test]
    fn new_convert_subnet_to_l1_tx_bytes_match_go() {
        let env = Env::default();
        let (_, pop0) = bls_pop(0x25);
        let (_, pop1) = bls_pop(0x26);

        let validators = vec![
            ConvertSubnetToL1Validator {
                node_id: vec![0xaa; 20],
                weight: 0x0102_0304_0506_0708,
                balance: AVAX,
                signer: pop0,
                remaining_balance_owner: PChainOwner {
                    threshold: 1,
                    addresses: vec![short_id(0x11)],
                },
                deactivation_owner: PChainOwner {
                    threshold: 1,
                    addresses: vec![short_id(0x22)],
                },
            },
            ConvertSubnetToL1Validator {
                node_id: vec![0xbb; 20],
                weight: 0x1112_1314_1516_1718,
                balance: 2 * AVAX,
                signer: pop1,
                remaining_balance_owner: PChainOwner::default(),
                deactivation_owner: PChainOwner::default(),
            },
        ];
        let tx = env
            .builder()
            .new_convert_subnet_to_l1_tx(
                subnet_id(),
                other_chain_id(),
                vec![0x5a; 32],
                validators,
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_convert_subnet_to_l1",
            ava_platformvm::txs::UnsignedTx::ConvertSubnetToL1(tx),
        );
    }

    #[test]
    fn new_register_l1_validator_tx_bytes_match_go() {
        let env = Env::default();
        let vector = load_vector("p_register_l1_validator");
        let message = hex::decode(&vector.inputs["warp_message"]).expect("warp hex");
        let proof: [u8; 96] = hex::decode(&vector.inputs["bls_pop_0"])
            .expect("pop hex")
            .try_into()
            .expect("pop len");

        let tx = env
            .builder()
            .new_register_l1_validator_tx(AVAX, proof, message, &env.opts())
            .expect("build");
        env.check(
            "p_register_l1_validator",
            ava_platformvm::txs::UnsignedTx::RegisterL1Validator(tx),
        );
    }

    #[test]
    fn new_set_l1_validator_weight_tx_bytes_match_go() {
        let env = Env::default();
        let vector = load_vector("p_set_l1_validator_weight");
        let message = hex::decode(&vector.inputs["warp_message"]).expect("warp hex");

        let tx = env
            .builder()
            .new_set_l1_validator_weight_tx(message, &env.opts())
            .expect("build");
        env.check(
            "p_set_l1_validator_weight",
            ava_platformvm::txs::UnsignedTx::SetL1ValidatorWeight(tx),
        );
    }

    #[test]
    fn new_increase_l1_validator_balance_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_increase_l1_validator_balance_tx(validation_id(), AVAX, &env.opts())
            .expect("build");
        env.check(
            "p_increase_l1_validator_balance",
            ava_platformvm::txs::UnsignedTx::IncreaseL1ValidatorBalance(tx),
        );
    }

    #[test]
    fn new_disable_l1_validator_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_disable_l1_validator_tx(validation_id(), &env.opts())
            .expect("build");
        env.check(
            "p_disable_l1_validator",
            ava_platformvm::txs::UnsignedTx::DisableL1Validator(tx),
        );
    }

    #[test]
    fn new_add_auto_renewed_validator_tx_bytes_match_go() {
        let keys = test_keys();
        let utxo_addr = keys[1].public_key().address();
        let env = Env::new(staker_utxos(utxo_addr), BTreeMap::new());
        let (_, pop) = bls_pop(0x25);

        let tx = env
            .builder()
            .new_add_auto_renewed_validator_tx(
                node_id(),
                2 * AVAX,
                PopSigner::ProofOfPossession(pop),
                env.subnet_owner.clone(),
                env.subnet_owner.clone(),
                env.validation_owner.clone(),
                1_000_000,
                500_000,
                7 * 24 * 3600,
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_add_auto_renewed_validator",
            ava_platformvm::txs::UnsignedTx::AddAutoRenewedValidator(tx),
        );
    }

    #[test]
    fn new_set_auto_renewed_validator_config_tx_bytes_match_go() {
        let env = Env::default();
        let tx = env
            .builder()
            .new_set_auto_renewed_validator_config_tx(
                validation_id(),
                750_000,
                14 * 24 * 3600,
                &env.opts(),
            )
            .expect("build");
        env.check(
            "p_set_auto_renewed_validator_config",
            ava_platformvm::txs::UnsignedTx::SetAutoRenewedValidatorConfig(tx),
        );
    }
}
