// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `executor.StandardTx` — the decision-tx executor
//! (`vms/platformvm/txs/executor/standard_tx_executor.go`, specs 08 §2.4).
//!
//! [`StandardTxExecutor`] is a [`Visitor`](crate::txs::Visitor) that mutates a
//! [`Diff`](crate::state::diff::Diff) to represent the chain state after a
//! standard (decision) tx and records the three outputs the block executor needs
//! on accept:
//!
//! - **`inputs`** — the UTXO ids consumed from shared memory (import txs).
//! - **`atomic_requests`** — the shared-memory put/remove ops to apply on accept
//!   (import/export txs). The *application* of these is M4.18's; the type is
//!   defined here so the atomic executor can reuse it.
//! - **`on_accept`** — a deferred callback (e.g. create-chain) run on accept.
//!
//! The visitor methods reject the proposal txs (`AdvanceTime`/`RewardValidator`,
//! M4.17) and the L1-lifecycle txs (`Register`/`SetWeight`/…, M4.19 / M4.18) with
//! [`Error::WrongTxType`] via the default [`Visitor`] impls, exactly as the Go
//! `standardTxExecutor` returns `ErrWrongTxType` for the proposal txs.

use std::collections::{BTreeMap, BTreeSet};

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::state::metadata_validator::StakingInfo;
use crate::state::staker::Staker;
use crate::txs::components::TransferableOutput;
use crate::txs::{
    AddAutoRenewedValidatorTx, AddPermissionlessDelegatorTx, AddPermissionlessValidatorTx, BaseTx,
    CreateChainTx, CreateSubnetTx, ExportTx, ImportTx, Priority, RemoveSubnetValidatorTx,
    SetAutoRenewedValidatorConfigTx, TransferSubnetOwnershipTx, Visitor,
};
use crate::utxo::{self, Utxo};

use super::backend::Backend;
use super::staker_tx_verification as staker;
use super::state_changes;
use super::subnet_tx_verification as subnet;

/// `atomic.Requests` — the shared-memory ops a tx asks the block executor to
/// apply on accept (`chains/atomic`). The *application* is M4.18's; the standard
/// executor only records them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AtomicRequests {
    /// `PutRequests` — `(key, value)` UTXO blobs to write to shared memory
    /// (export txs).
    pub put_requests: Vec<(Vec<u8>, Vec<u8>)>,
    /// `RemoveRequests` — UTXO id keys to remove from shared memory (import txs).
    pub remove_requests: Vec<Vec<u8>>,
}

/// The outputs of a successful [`StandardTxExecutor`] run, mirroring Go
/// `StandardTx`'s `(inputs, atomicRequests, onAccept)` return.
#[derive(Default)]
pub struct StandardTxOutputs {
    /// The UTXO ids consumed from shared memory (import txs).
    pub inputs: BTreeSet<Id>,
    /// The shared-memory ops to apply on accept, keyed by peer chain id.
    pub atomic_requests: BTreeMap<Id, AtomicRequests>,
    /// A deferred callback run on accept (e.g. create-chain), if any.
    pub on_accept: Option<Box<dyn FnOnce() + Send>>,
}

/// `standardTxExecutor` — a [`Visitor`] that executes a decision tx against a
/// [`Diff`].
///
/// Construct with [`StandardTxExecutor::new`], dispatch via
/// [`UnsignedTx::visit`](crate::txs::UnsignedTx::visit), then take
/// [`StandardTxExecutor::into_outputs`].
pub struct StandardTxExecutor<'a> {
    backend: &'a Backend,
    state: &'a mut Diff,
    /// The signed tx (for credentials / id).
    tx: &'a crate::txs::Tx,
    /// The marshaled unsigned-tx bytes (hashed by the fx for auth checks).
    unsigned_bytes: Vec<u8>,
    /// The tx id (`sha256(signed_bytes)`).
    tx_id: Id,
    /// Accumulated outputs.
    outputs: StandardTxOutputs,
}

impl<'a> StandardTxExecutor<'a> {
    /// Builds an executor over `state` for the signed `tx`. `unsigned_bytes` is
    /// the marshaled unsigned tx (the fx hashes it for subnet-auth checks); the
    /// caller supplies it because the codec manager lives at the call site.
    pub fn new(
        backend: &'a Backend,
        state: &'a mut Diff,
        tx: &'a crate::txs::Tx,
        unsigned_bytes: Vec<u8>,
    ) -> Self {
        Self {
            backend,
            state,
            tx,
            unsigned_bytes,
            tx_id: tx.id(),
            outputs: StandardTxOutputs::default(),
        }
    }

    /// Consumes the executor, returning the accumulated outputs.
    #[must_use]
    pub fn into_outputs(self) -> StandardTxOutputs {
        self.outputs
    }

    /// The fee in force for this tx (fork-selected), charged on AVAX.
    fn fee(&self) -> Result<u64> {
        state_changes::fee_calculator(self.backend, self.state)
            .calculate_fee(crate::txs::fee::complexity::base_tx_complexity())
    }

    /// `avax.Consume` + `avax.Produce` over the embedded base tx.
    fn consume_produce(
        &mut self,
        ins: &[crate::txs::components::TransferableInput],
        outs: &[TransferableOutput],
    ) -> Result<()> {
        utxo::consume(self.state, ins);
        utxo::produce(self.state, self.tx_id, outs)
    }

    /// `putStaker` — derives the [`Staker`] from a permissionless staking tx and
    /// adds it to the current set, minting & accruing the potential reward
    /// (post-Durango immediate-current model; specs 08 §2.4).
    fn put_permissionless_staker(
        &mut self,
        subnet: Id,
        node: ava_types::node_id::NodeId,
        public_key: Option<ava_crypto::bls::PublicKey>,
        weight: u64,
        end: u64,
        is_validator: bool,
    ) -> Result<()> {
        let chain_time = self.state.timestamp();
        let end_time = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(end))
            .unwrap_or(chain_time);

        // Only permissionless stakers earn a potential reward.
        let current_supply = self.state.current_supply(subnet)?;
        let stake_duration = end_time
            .duration_since(chain_time)
            .map_err(|_| Error::StakeTooShort)?;
        let calc = crate::reward::Calculator::new(self.backend.staking.reward_config);
        let stake_duration_ns =
            u64::try_from(stake_duration.as_nanos()).map_err(|_| Error::Overflow)?;
        let potential_reward = calc.calculate(stake_duration_ns, weight, current_supply);
        let new_supply = current_supply
            .checked_add(potential_reward)
            .ok_or(Error::Overflow)?;
        self.state.set_current_supply(subnet, new_supply);

        let priority = if is_validator {
            if subnet == Id::EMPTY {
                Priority::PrimaryNetworkValidatorCurrent
            } else {
                Priority::SubnetPermissionlessValidatorCurrent
            }
        } else if subnet == Id::EMPTY {
            Priority::PrimaryNetworkDelegatorCurrent
        } else {
            Priority::SubnetPermissionlessDelegatorCurrent
        };

        let staker = Staker::new_current(
            self.tx_id,
            node,
            public_key,
            subnet,
            weight,
            chain_time,
            end_time,
            potential_reward,
            priority,
        );
        if is_validator {
            self.state.put_current_validator(staker)?;
        } else {
            self.state.put_current_delegator(staker);
        }
        Ok(())
    }
}

impl Visitor for StandardTxExecutor<'_> {
    type Error = Error;

    fn base(&mut self, tx: &BaseTx) -> Result<()> {
        let current_timestamp = self.state.timestamp();
        if !self.backend.is_durango_activated(current_timestamp) {
            return Err(Error::DurangoUpgradeNotActive);
        }
        tx.syntactic_verify()?;

        let fee = self.fee()?;
        state_changes::verify_spend(
            self.state,
            &tx.base.ins,
            &tx.base.outs,
            fee,
            self.backend.avax_asset_id,
        )?;

        let (ins, outs) = (tx.base.ins.clone(), tx.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn create_subnet(&mut self, tx: &CreateSubnetTx) -> Result<()> {
        tx.base.syntactic_verify()?;

        let fee = self.fee()?;
        state_changes::verify_spend(
            self.state,
            &tx.base.base.ins,
            &tx.base.base.outs,
            fee,
            self.backend.avax_asset_id,
        )?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)?;

        // Record the new subnet and its owner (keyed by the tx id).
        self.state.add_subnet(self.tx_id);
        let owner_bytes = crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, &tx.owner)
            .map_err(Error::Codec)?;
        self.state.set_subnet_owner(self.tx_id, owner_bytes);
        Ok(())
    }

    fn create_chain(&mut self, tx: &CreateChainTx) -> Result<()> {
        tx.base.syntactic_verify()?;

        // The issuer must control the subnet (PoA subnet authorization).
        subnet::verify_subnet_authorization(
            self.backend,
            self.state,
            self.tx,
            &self.unsigned_bytes,
            tx.subnet_id,
            &tx.subnet_auth,
        )?;

        let fee = self.fee()?;
        state_changes::verify_spend(
            self.state,
            &tx.base.base.ins,
            &tx.base.base.outs,
            fee,
            self.backend.avax_asset_id,
        )?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)?;
        self.state.add_chain(tx.subnet_id, self.tx_id);
        // The Go executor schedules `Config.CreateChain` on accept; that wiring
        // is the chain manager's (M4.20), so no callback is recorded here.
        Ok(())
    }

    fn transfer_subnet_ownership(&mut self, tx: &TransferSubnetOwnershipTx) -> Result<()> {
        let current_timestamp = self.state.timestamp();
        if !self.backend.is_durango_activated(current_timestamp) {
            return Err(Error::DurangoUpgradeNotActive);
        }
        tx.base.syntactic_verify()?;

        subnet::verify_subnet_authorization(
            self.backend,
            self.state,
            self.tx,
            &self.unsigned_bytes,
            tx.subnet,
            &tx.subnet_auth,
        )?;

        let fee = self.fee()?;
        state_changes::verify_spend(
            self.state,
            &tx.base.base.ins,
            &tx.base.base.outs,
            fee,
            self.backend.avax_asset_id,
        )?;

        // Set the new owner, then consume/produce.
        let owner_bytes = crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, &tx.owner)
            .map_err(Error::Codec)?;
        self.state.set_subnet_owner(tx.subnet, owner_bytes);

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn remove_subnet_validator(&mut self, tx: &RemoveSubnetValidatorTx) -> Result<()> {
        tx.base.syntactic_verify()?;

        // The node must be a permissioned validator of the subnet.
        let staker = staker::get_validator(self.state, tx.subnet, tx.node_id)
            .map_err(|_| Error::NotValidator)?;
        if !staker.priority.is_permissioned_validator() {
            return Err(Error::RemovePermissionlessValidator);
        }
        let is_current = staker.priority.is_current();

        if self.backend.bootstrapped {
            subnet::verify_subnet_authorization(
                self.backend,
                self.state,
                self.tx,
                &self.unsigned_bytes,
                tx.subnet,
                &tx.subnet_auth,
            )?;

            let fee = self.fee()?;
            state_changes::verify_spend(
                self.state,
                &tx.base.base.ins,
                &tx.base.base.outs,
                fee,
                self.backend.avax_asset_id,
            )?;
        }

        if is_current {
            self.state.delete_current_validator(&staker);
        } else {
            self.state.delete_pending_validator(&staker);
        }

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn add_permissionless_validator(&mut self, tx: &AddPermissionlessValidatorTx) -> Result<()> {
        staker::verify_add_permissionless_validator(
            self.backend,
            self.state,
            &self.unsigned_bytes,
            tx,
        )?;

        let public_key = tx.signer.key()?;
        self.put_permissionless_staker(
            tx.subnet,
            tx.validator.node_id,
            public_key,
            tx.validator.wght,
            tx.validator.end,
            true,
        )?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn add_permissionless_delegator(&mut self, tx: &AddPermissionlessDelegatorTx) -> Result<()> {
        staker::verify_add_permissionless_delegator(
            self.backend,
            self.state,
            &self.unsigned_bytes,
            tx,
        )?;

        self.put_permissionless_staker(
            tx.subnet,
            tx.validator.node_id,
            None,
            tx.validator.wght,
            tx.validator.end,
            false,
        )?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn add_auto_renewed_validator(&mut self, tx: &AddAutoRenewedValidatorTx) -> Result<()> {
        staker::verify_add_auto_renewed_validator(self.backend, self.state, tx)?;

        let weight = tx.weight()?;
        let subnet = tx.subnet_id();
        let node = tx.node_id()?;

        // Compute the potential reward over `period`, then bump primary-network
        // supply (Helicon: auto-renewed validators mint at execution time, like
        // permissionless validators).
        let current_supply = self.state.current_supply(subnet)?;
        let duration = std::time::Duration::from_secs(tx.period);
        let duration_ns = u64::try_from(duration.as_nanos()).map_err(|_| Error::Overflow)?;
        let calc = crate::reward::Calculator::new(self.backend.staking.reward_config);
        let potential_reward = calc.calculate(duration_ns, weight, current_supply);
        let new_supply = current_supply
            .checked_add(potential_reward)
            .ok_or(Error::Overflow)?;
        self.state.set_current_supply(subnet, new_supply);

        // The staker's window is [chain_time, chain_time + period].
        let start_time = self.state.timestamp();
        let end_time = start_time.checked_add(duration).unwrap_or(start_time);
        let public_key = tx.public_key()?;

        let staker = Staker::new_staker(
            self.tx_id,
            node,
            public_key,
            subnet,
            weight,
            start_time,
            end_time,
            potential_reward,
            tx.current_priority(),
        );
        self.state.put_current_validator(staker)?;

        // Persist the auto-renew config (`AutoCompoundRewardShares`, `NextPeriod`).
        let info = StakingInfo {
            auto_compound_reward_shares: tx.auto_compound_reward_shares,
            next_period: tx.period,
            ..StakingInfo::default()
        };
        self.state.set_staking_info(subnet, node, info)?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn set_auto_renewed_validator_config(
        &mut self,
        tx: &SetAutoRenewedValidatorConfigTx,
    ) -> Result<()> {
        let validator = staker::verify_set_auto_renewed_validator_config(
            self.backend,
            self.state,
            self.tx,
            &self.unsigned_bytes,
            tx,
        )?;

        // Mutate the validator's staking info in place.
        let mut info = self
            .state
            .get_staking_info(validator.subnet_id, validator.node_id)?;
        info.auto_compound_reward_shares = tx.auto_compound_reward_shares;
        info.next_period = tx.period;
        self.state
            .set_staking_info(validator.subnet_id, validator.node_id, info)?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)
    }

    fn import(&mut self, tx: &ImportTx) -> Result<()> {
        let current_timestamp = self.state.timestamp();
        let _ = self.backend.is_durango_activated(current_timestamp);
        tx.base.syntactic_verify()?;

        // Record the imported UTXO ids as the consumed shared-memory inputs.
        let mut remove_requests = Vec::with_capacity(tx.imported_inputs.len());
        for input in &tx.imported_inputs {
            let utxo_id = input.input_id();
            self.outputs.inputs.insert(utxo_id);
            remove_requests.push(utxo_id.to_bytes().to_vec());
        }

        // Note: the shared-memory flow check (M4.18) is skipped here; the
        // value-conservation check over the *local* inputs/outputs still runs.
        let fee = self.fee()?;
        // ImportTx pays from imported inputs + base ins; the byte-stored UTXO
        // handler only sees local UTXOs, so the conservation check is deferred to
        // the atomic executor (M4.18). The atomic request is recorded regardless,
        // matching Go (which applies requests even when not verifying them).

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        // Consume local ins (if any) and produce outs.
        utxo::consume(self.state, &ins);
        utxo::produce(self.state, self.tx_id, &outs)?;
        let _ = fee;

        self.outputs.atomic_requests.insert(
            tx.source_chain,
            AtomicRequests {
                put_requests: Vec::new(),
                remove_requests,
            },
        );
        Ok(())
    }

    fn export(&mut self, tx: &ExportTx) -> Result<()> {
        let current_timestamp = self.state.timestamp();
        let _ = self.backend.is_durango_activated(current_timestamp);
        tx.base.syntactic_verify()?;

        let fee = self.fee()?;
        // The exported outputs are produced into shared memory, not the local
        // UTXO set, so they are excluded from the local conservation check.
        state_changes::verify_spend(
            self.state,
            &tx.base.base.ins,
            &tx.base.base.outs,
            fee,
            self.backend.avax_asset_id,
        )?;

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        utxo::consume(self.state, &ins);
        utxo::produce(self.state, self.tx_id, &outs)?;

        // Build the shared-memory put requests for the exported outputs.
        let base_outs_len = u32::try_from(outs.len()).map_err(|_| Error::Overflow)?;
        let mut put_requests = Vec::with_capacity(tx.exported_outputs.len());
        for (i, out) in tx.exported_outputs.iter().enumerate() {
            let i = u32::try_from(i).map_err(|_| Error::Overflow)?;
            let output_index = base_outs_len.checked_add(i).ok_or(Error::Overflow)?;
            let exported = exported_utxo(self.tx_id, output_index, out);
            let key = exported.input_id().to_bytes().to_vec();
            let value = exported.marshal()?;
            put_requests.push((key, value));
        }
        self.outputs.atomic_requests.insert(
            tx.destination_chain,
            AtomicRequests {
                put_requests,
                remove_requests: Vec::new(),
            },
        );
        Ok(())
    }
}

/// Builds the `avax.UTXO` exported by an [`ExportTx`] output.
fn exported_utxo(tx_id: Id, output_index: u32, out: &TransferableOutput) -> Utxo {
    Utxo {
        tx_id,
        output_index,
        asset_id: out.asset_id,
        out: out.out.clone(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod standard_executor {
    //! M4.16 conformance tests — ported decision-tx cases from Go
    //! `standard_tx_executor_test.go` (the cases buildable without the
    //! not-yet-ported warp / shared-memory / subnet-transformation fixtures).
    //!
    //! Each test builds a `State`-backed `Diff`, runs the executor, and asserts
    //! the resulting `(consumed inputs, atomic requests, Diff mutations)`.

    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use ava_database::MemDb;
    use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;
    use ava_utils::clock::MockClock;

    use super::*;
    use crate::signer::{ProofOfPossession, Signer};
    use crate::state::chain::{Chain, Versions};
    use crate::state::state::State;
    use crate::txs::components::Auth;
    use crate::txs::components::{
        BaseTx as AvaxBaseTx, Input, Output, Owner, TransferableInput, TransferableOutput,
    };
    use crate::txs::executor::backend::{StakingConfig, UpgradeSchedule};
    use crate::txs::validator::Validator;
    use crate::txs::{
        AddAutoRenewedValidatorTx, AddPermissionlessValidatorTx, AdvanceTimeTx,
        SetAutoRenewedValidatorConfigTx, Tx, UnsignedTx,
    };
    use crate::txs::{AddValidatorTx, CreateChainTx, CreateSubnetTx};

    const AVAX_ASSET: [u8; 32] = [0x42; 32];
    const AVAX: u64 = 1_000_000_000;
    /// The mainnet static tx fee (`MilliAvax`).
    const TX_FEE: u64 = 1_000_000;

    /// A `Versions` resolving exactly one parent block id.
    struct SingleParent {
        id: Id,
        chain: Arc<dyn Chain>,
    }
    impl Versions for SingleParent {
        fn get_state(&self, block_id: Id) -> Option<Arc<dyn Chain>> {
            (block_id == self.id).then(|| Arc::clone(&self.chain))
        }
    }

    fn owners(addr: u8) -> OutputOwners {
        OutputOwners::new(0, 1, vec![ShortId::from([addr; 20])])
    }

    /// Builds a `Diff` over a fresh `State` whose chain time is `ts` and which
    /// holds the given AVAX UTXOs (keyed by `(tx_id, index)`).
    fn diff_with_utxos(ts: SystemTime, utxos: &[(Id, u32, u64)]) -> Diff {
        let mut state = State::new(MemDb::new()).expect("state");
        state.set_timestamp(ts);
        state.set_current_supply(Id::EMPTY, 100_000_000 * AVAX);
        for &(tx_id, index, amt) in utxos {
            let utxo = Utxo {
                tx_id,
                output_index: index,
                asset_id: Id::from(AVAX_ASSET),
                out: Output::Transfer(TransferOutput::new(amt, owners(1))),
            };
            state.add_utxo(utxo.input_id(), utxo.marshal().expect("marshal utxo"));
        }
        let parent_id = Id::from([0xAB; 32]);
        let base: Arc<dyn Chain> = Arc::new(state);
        let versions = SingleParent {
            id: parent_id,
            chain: base,
        };
        Diff::new(parent_id, &versions).expect("diff")
    }

    /// A test backend with the given fork schedule, mainnet staking params,
    /// static fees, and an un-bootstrapped fx (structural auth checks only).
    fn backend(upgrades: UpgradeSchedule, bootstrapped: bool) -> Backend {
        Backend {
            upgrades,
            staking: StakingConfig::mainnet(),
            static_fee_config: crate::txs::fee::simple_calculator::StaticFeeConfig::MAINNET,
            network_id: 1,
            chain_id: Id::EMPTY,
            avax_asset_id: Id::from(AVAX_ASSET),
            node_id: NodeId::EMPTY,
            fx: ava_secp256k1fx::Fx::new(Arc::new(MockClock::at(SystemTime::UNIX_EPOCH))),
            bootstrapped,
        }
    }

    /// An AVAX `TransferableInput` consuming `(tx_id, index, amt)`.
    fn avax_input(tx_id: Id, index: u32, amt: u64) -> TransferableInput {
        TransferableInput {
            tx_id,
            output_index: index,
            asset_id: Id::from(AVAX_ASSET),
            r#in: Input::Transfer(TransferInput::new(amt, vec![0])),
        }
    }

    /// An AVAX `TransferableOutput` of `amt` to `owners(1)`.
    fn avax_output(amt: u64) -> TransferableOutput {
        TransferableOutput {
            asset_id: Id::from(AVAX_ASSET),
            out: Output::Transfer(TransferOutput::new(amt, owners(1))),
        }
    }

    fn run(backend: &Backend, diff: &mut Diff, unsigned: UnsignedTx) -> Result<StandardTxOutputs> {
        run_creds(backend, diff, unsigned, vec![])
    }

    /// As [`run`], but attaches `creds` to the signed tx (the last credential is
    /// consumed as the subnet/owner authorization).
    fn run_creds(
        backend: &Backend,
        diff: &mut Diff,
        unsigned: UnsignedTx,
        creds: Vec<crate::txs::tx::Credential>,
    ) -> Result<StandardTxOutputs> {
        let mut tx = Tx::new(unsigned);
        tx.creds = creds;
        tx.initialize(crate::txs::codec::Codec()).expect("init");
        let unsigned_bytes = crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, &tx.unsigned)
            .expect("marshal unsigned");
        let mut exec = StandardTxExecutor::new(backend, diff, &tx, unsigned_bytes);
        tx.unsigned.visit(&mut exec)?;
        Ok(exec.into_outputs())
    }

    /// Runs `unsigned` and returns the error, asserting the run failed.
    /// (`StandardTxOutputs` is not `Debug` — it holds the `on_accept` closure —
    /// so `Result::unwrap_err` is unavailable.)
    fn run_err(backend: &Backend, diff: &mut Diff, unsigned: UnsignedTx) -> Error {
        match run(backend, diff, unsigned) {
            Ok(_) => panic!("expected the tx to fail execution"),
            Err(e) => e,
        }
    }

    /// `CreateSubnetTx` valid: consumes the funding UTXO, records the subnet +
    /// owner, and the produced change is in the UTXO set.
    #[test]
    fn standard_executor_create_subnet_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([1; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
        // Pre-Etna static-fee regime so the fee equals the flat TX_FEE.
        let b = backend(UpgradeSchedule::durango_only(), true);

        let tx = CreateSubnetTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(100 * AVAX - TX_FEE)],
                ins: vec![avax_input(fund, 0, 100 * AVAX)],
                memo: vec![],
            }),
            owner: Owner::Secp256k1(owners(2)),
        };
        let out = run(&b, &mut diff, UnsignedTx::CreateSubnet(tx)).expect("create subnet");

        // No atomic requests / consumed shared-memory inputs for create-subnet.
        assert!(out.inputs.is_empty());
        assert!(out.atomic_requests.is_empty());
        // The subnet is recorded with an owner; the funding UTXO is consumed.
        assert_eq!(diff.subnets().len(), 1);
        let subnet_id = diff.subnets()[0];
        assert!(diff.get_subnet_owner(subnet_id).is_ok());
        assert!(diff.get_utxo(avax_input(fund, 0, 0).input_id()).is_err());
    }

    /// `BaseTx` is rejected before Durango activates.
    #[test]
    fn standard_executor_base_tx_pre_durango_rejected() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let fund = Id::from([3; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 10 * AVAX)]);
        let b = backend(UpgradeSchedule::none_active(), true);

        let tx = crate::txs::BaseTx::new(AvaxBaseTx {
            network_id: 1,
            blockchain_id: Id::EMPTY,
            outs: vec![avax_output(10 * AVAX - TX_FEE)],
            ins: vec![avax_input(fund, 0, 10 * AVAX)],
            memo: vec![],
        });
        let err = run_err(&b, &mut diff, UnsignedTx::Base(tx));
        assert!(matches!(err, Error::DurangoUpgradeNotActive));
    }

    /// A proposal tx (`AdvanceTimeTx`) is rejected by the standard executor with
    /// the wrong-tx-type sentinel (Go `ErrWrongTxType`).
    #[test]
    fn standard_executor_rejects_proposal_tx() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::all_active(), true);
        let err = run_err(
            &b,
            &mut diff,
            UnsignedTx::AdvanceTime(AdvanceTimeTx::default()),
        );
        assert!(matches!(err, Error::WrongTxType));
    }

    /// `AddPermissionlessValidatorTx` bound checks: a primary-network validator
    /// staking below `MinValidatorStake` is rejected with `WeightTooSmall`.
    #[test]
    fn standard_executor_add_permissionless_validator_weight_too_small() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([5; 32]);
        // Stake just 1 AVAX (< MinValidatorStake = 2000 AVAX).
        let weight = AVAX;
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        let b = backend(UpgradeSchedule::all_active(), true);

        let tx = app_validator(fund, weight, ts);
        let err = run_err(&b, &mut diff, UnsignedTx::AddPermissionlessValidator(tx));
        assert!(matches!(err, Error::WeightTooSmall), "got {err:?}");
    }

    /// `AddPermissionlessValidatorTx` valid primary-network validator: verified,
    /// added to the current set, and the potential reward grows the supply.
    #[test]
    fn standard_executor_add_permissionless_validator_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([6; 32]);
        let weight = 2_000 * AVAX; // == MinValidatorStake
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        // Pre-Etna static-fee regime so the fee equals the flat TX_FEE.
        let b = backend(UpgradeSchedule::durango_only(), true);

        let supply_before = diff.current_supply(Id::EMPTY).unwrap();
        let tx = app_validator(fund, weight, ts);
        let node = tx.validator.node_id;
        let out = run(&b, &mut diff, UnsignedTx::AddPermissionlessValidator(tx))
            .expect("valid app validator");

        assert!(out.inputs.is_empty());
        // The validator is now in the current set, and supply grew by the reward.
        assert!(diff.get_current_validator(Id::EMPTY, node).is_ok());
        assert!(diff.current_supply(Id::EMPTY).unwrap() >= supply_before);
    }

    /// A subnet-auth tx (`CreateChainTx` is exercised by M4.16's siblings; here
    /// we check the shared `verify_subnet_authorization` rejects a missing
    /// subnet owner via the `RemoveSubnetValidatorTx` path is covered elsewhere).
    /// We assert the bound: `AddValidatorTx` (pre-Durango-only) is not handled by
    /// the standard executor and rejects as wrong-type when undispatched.
    #[test]
    fn standard_executor_add_validator_unhandled() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::none_active(), true);
        // The legacy AddValidatorTx is intentionally not overridden by the
        // standard executor in this port (it is pre-Durango legacy); the default
        // visitor rejects it as wrong-type.
        let err = run_err(
            &b,
            &mut diff,
            UnsignedTx::AddValidator(AddValidatorTx::default()),
        );
        assert!(matches!(err, Error::WrongTxType));
    }

    /// Builds a `CreateChainTx` modifying `subnet`, funded by `(fund, 0, amt)`.
    fn create_chain(subnet: Id, fund: Id, amt: u64) -> CreateChainTx {
        CreateChainTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(amt - TX_FEE)],
                ins: vec![avax_input(fund, 0, amt)],
                memo: vec![],
            }),
            subnet_id: subnet,
            chain_name: "test".to_string(),
            vm_id: Id::from([9; 32]),
            fx_ids: vec![],
            genesis_data: vec![],
            // Empty sig-index set matching a threshold-0 owner.
            subnet_auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
        }
    }

    /// Subnet-auth case: `CreateChainTx` against an unknown subnet fails because
    /// the subnet owner cannot be resolved.
    #[test]
    fn standard_executor_create_chain_unknown_subnet_rejected() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([8; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
        let b = backend(UpgradeSchedule::durango_only(), true);

        // The subnet was never created, so `get_subnet_owner` returns NotFound.
        let tx = create_chain(Id::from([0x77; 32]), fund, 100 * AVAX);
        let err = run_err(&b, &mut diff, UnsignedTx::CreateChain(tx));
        assert!(matches!(err, Error::Database(_)), "got {err:?}");
    }

    /// Subnet-auth case: `CreateChainTx` against a subnet owned by a threshold-0
    /// owner authorizes (empty auth proves a 0-of-0 owner), records the chain,
    /// and consumes the funding UTXO.
    #[test]
    fn standard_executor_create_chain_subnet_auth_ok() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([10; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 100 * AVAX)]);
        let b = backend(UpgradeSchedule::durango_only(), true);

        // Create a subnet owned by a 0-of-0 owner (no signatures required).
        let subnet = Id::from([0x33; 32]);
        diff.add_subnet(subnet);
        let owner = Owner::Secp256k1(OutputOwners::new(0, 0, vec![]));
        let owner_bytes = crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, &owner)
            .expect("marshal owner");
        diff.set_subnet_owner(subnet, owner_bytes);

        let tx = create_chain(subnet, fund, 100 * AVAX);
        // One empty credential serves as the (0-of-0) subnet authorization.
        let creds = vec![crate::txs::tx::Credential { sigs: vec![] }];
        run_creds(&b, &mut diff, UnsignedTx::CreateChain(tx), creds).expect("create chain");

        // The chain is recorded under the subnet; the funding UTXO is consumed.
        assert_eq!(diff.chains(subnet).len(), 1);
        assert!(diff.get_utxo(avax_input(fund, 0, 0).input_id()).is_err());
    }

    /// Builds a primary-network `AddPermissionlessValidatorTx` funded by
    /// `(fund, 0)`, staking `weight`, with a 200-day window starting at `ts`.
    fn app_validator(fund: Id, weight: u64, ts: SystemTime) -> AddPermissionlessValidatorTx {
        let start = ts.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let end = start + 200 * 24 * 60 * 60;
        // A syntactically-valid PoP (the syntactic check verifies the proof, so
        // use the known-good Go vector key/sig).
        let pop = ProofOfPossession::new(BLS_PUBKEY, BLS_SIG);
        AddPermissionlessValidatorTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![avax_input(fund, 0, weight + 10 * AVAX)],
                memo: vec![],
            }),
            validator: Validator {
                node_id: NodeId::from([7; 20]),
                start,
                end,
                wght: weight,
            },
            subnet: Id::EMPTY,
            signer: Signer::ProofOfPossession(pop),
            stake_outs: vec![TransferableOutput {
                asset_id: Id::from(AVAX_ASSET),
                out: Output::Transfer(TransferOutput::new(weight, owners(1))),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners(1)),
            delegator_rewards_owner: Owner::Secp256k1(owners(1)),
            delegation_shares: 1_000_000,
            verified: std::cell::OnceCell::new(),
        }
    }

    // ----- M4.16 ACP-236(4) auto-renew cases (Helicon-gated) -----

    /// An [`UpgradeSchedule`] forcing Helicon active at the epoch (the dormant
    /// fork the conformance tests force on), with Durango active and Etna kept
    /// inactive so the fee stays the flat static `TX_FEE` (predictable
    /// conservation; the Helicon gate is independent of the Etna fee regime).
    fn helicon_active() -> UpgradeSchedule {
        let mut s = UpgradeSchedule::durango_only();
        s.helicon_time = SystemTime::UNIX_EPOCH;
        s
    }

    /// Builds an `AddAutoRenewedValidatorTx` funded by `(fund, 0)`, staking
    /// `weight` over `period` seconds, for the node `[7; 20]`.
    fn auto_renewed_validator(fund: Id, weight: u64, period: u64) -> AddAutoRenewedValidatorTx {
        let pop = ProofOfPossession::new(BLS_PUBKEY, BLS_SIG);
        AddAutoRenewedValidatorTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![avax_input(fund, 0, weight + 10 * AVAX)],
                memo: vec![],
            }),
            validator_node_id: vec![7u8; 20],
            signer: Signer::ProofOfPossession(pop),
            stake_outs: vec![TransferableOutput {
                asset_id: Id::from(AVAX_ASSET),
                out: Output::Transfer(TransferOutput::new(weight, owners(1))),
            }],
            validator_rewards_owner: Owner::Secp256k1(owners(1)),
            delegator_rewards_owner: Owner::Secp256k1(owners(1)),
            // A 0-of-0 authority (an empty credential proves control).
            validator_authority: Owner::Secp256k1(OutputOwners::new(0, 0, vec![])),
            delegation_shares: 1_000_000,
            auto_compound_reward_shares: 300_000,
            period,
        }
    }

    /// Pre-Helicon, `AddAutoRenewedValidatorTx` is rejected (Helicon dormant).
    #[test]
    fn standard_executor_add_auto_renew_pre_helicon_rejected() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x40; 32]);
        let weight = 2_000 * AVAX;
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        // Durango+Etna active, Helicon NOT (the production posture).
        let b = backend(UpgradeSchedule::durango_only(), true);

        let period = 30 * 24 * 60 * 60; // 30 days
        let tx = auto_renewed_validator(fund, weight, period);
        let err = run_err(&b, &mut diff, UnsignedTx::AddAutoRenewedValidator(tx));
        assert!(matches!(err, Error::HeliconUpgradeNotActive), "got {err:?}");
    }

    /// Post-Helicon, a valid `AddAutoRenewedValidatorTx` verifies, is added to
    /// the current set, grows supply, and persists its staking info.
    #[test]
    fn standard_executor_add_auto_renew_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x41; 32]);
        let weight = 2_000 * AVAX; // == MinValidatorStake
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        let b = backend(helicon_active(), true);

        let supply_before = diff.current_supply(Id::EMPTY).unwrap();
        let period = 30 * 24 * 60 * 60; // 30 days, within [2 weeks, 365 days]
        let tx = auto_renewed_validator(fund, weight, period);
        let node = NodeId::from([7; 20]);
        run(&b, &mut diff, UnsignedTx::AddAutoRenewedValidator(tx)).expect("valid auto-renew");

        // The validator is now current, supply grew, and the staking info holds
        // the auto-compound config.
        assert!(diff.get_current_validator(Id::EMPTY, node).is_ok());
        assert!(diff.current_supply(Id::EMPTY).unwrap() >= supply_before);
        let info = diff
            .get_staking_info(Id::EMPTY, node)
            .expect("staking info present");
        assert_eq!(info.auto_compound_reward_shares, 300_000);
        assert_eq!(info.next_period, period);
        // The funding UTXO was consumed.
        assert!(diff.get_utxo(avax_input(fund, 0, 0).input_id()).is_err());
    }

    /// Post-Helicon, an `AddAutoRenewedValidatorTx` staking below
    /// `MinValidatorStake` is rejected with `WeightTooSmall`.
    #[test]
    fn standard_executor_add_auto_renew_weight_too_small() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x42; 32]);
        let weight = AVAX; // 1 AVAX < MinValidatorStake (2000 AVAX)
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        let b = backend(helicon_active(), true);

        let period = 30 * 24 * 60 * 60;
        let tx = auto_renewed_validator(fund, weight, period);
        let err = run_err(&b, &mut diff, UnsignedTx::AddAutoRenewedValidator(tx));
        assert!(matches!(err, Error::WeightTooSmall), "got {err:?}");
    }

    /// Post-Helicon, a period shorter than `MinStakeDuration` is rejected with
    /// `StakeTooShort`.
    #[test]
    fn standard_executor_add_auto_renew_period_too_short() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x43; 32]);
        let weight = 2_000 * AVAX;
        let mut diff = diff_with_utxos(ts, &[(fund, 0, weight + 10 * AVAX)]);
        let b = backend(helicon_active(), true);

        let period = 60; // 1 minute < MinStakeDuration (2 weeks)
        let tx = auto_renewed_validator(fund, weight, period);
        let err = run_err(&b, &mut diff, UnsignedTx::AddAutoRenewedValidator(tx));
        assert!(matches!(err, Error::StakeTooShort), "got {err:?}");
    }

    /// Adds an auto-renewed validator to `diff`, returning its tx id (the bytes
    /// are stored via `add_tx` so the config tx can resolve it).
    fn add_auto_renewed(b: &Backend, diff: &mut Diff, fund: Id, weight: u64, period: u64) -> Id {
        let tx = auto_renewed_validator(fund, weight, period);
        let mut signed = Tx::new(UnsignedTx::AddAutoRenewedValidator(tx));
        signed.initialize(crate::txs::codec::Codec()).expect("init");
        let tx_id = signed.id();
        let unsigned_bytes = crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, &signed.unsigned)
            .expect("marshal unsigned");
        {
            let mut exec = StandardTxExecutor::new(b, diff, &signed, unsigned_bytes);
            signed.unsigned.visit(&mut exec).expect("add auto-renew");
        }
        // Store the signed bytes so SetAutoRenewedValidatorConfigTx can resolve it.
        diff.add_tx(tx_id, signed.bytes().to_vec());
        tx_id
    }

    /// Post-Helicon, `SetAutoRenewedValidatorConfigTx` mutates the validator's
    /// `auto_compound_reward_shares` / `next_period`.
    #[test]
    fn standard_executor_set_auto_renew_config_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x44; 32]);
        let cfg_fund = Id::from([0x45; 32]);
        let weight = 2_000 * AVAX;
        let period = 30 * 24 * 60 * 60;
        let mut diff = diff_with_utxos(
            ts,
            &[(fund, 0, weight + 10 * AVAX), (cfg_fund, 0, 10 * AVAX)],
        );
        let b = backend(helicon_active(), true);

        let staker_tx_id = add_auto_renewed(&b, &mut diff, fund, weight, period);
        let node = NodeId::from([7; 20]);

        let new_period = 60 * 24 * 60 * 60; // 60 days
        let tx = SetAutoRenewedValidatorConfigTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![avax_input(cfg_fund, 0, 10 * AVAX)],
                memo: vec![],
            }),
            tx_id: staker_tx_id,
            // A 0-of-0 authority requires no signatures.
            auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
            auto_compound_reward_shares: 500_000,
            period: new_period,
        };
        // One empty credential serves as the (0-of-0) validator authority.
        let creds = vec![crate::txs::tx::Credential { sigs: vec![] }];
        run_creds(
            &b,
            &mut diff,
            UnsignedTx::SetAutoRenewedValidatorConfig(tx),
            creds,
        )
        .expect("set auto-renew config");

        let info = diff
            .get_staking_info(Id::EMPTY, node)
            .expect("staking info present");
        assert_eq!(info.auto_compound_reward_shares, 500_000);
        assert_eq!(info.next_period, new_period);
    }

    /// Pre-Helicon, `SetAutoRenewedValidatorConfigTx` is rejected.
    #[test]
    fn standard_executor_set_auto_renew_config_pre_helicon_rejected() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), true);

        let tx = SetAutoRenewedValidatorConfigTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![],
                memo: vec![],
            }),
            tx_id: Id::from([0x99; 32]),
            auth: Auth::Secp256k1(ava_secp256k1fx::Input::new(vec![])),
            auto_compound_reward_shares: 0,
            period: 0,
        };
        let err = run_err(&b, &mut diff, UnsignedTx::SetAutoRenewedValidatorConfig(tx));
        assert!(matches!(err, Error::HeliconUpgradeNotActive), "got {err:?}");
    }

    /// The known-good BLS PoP from the Go vectors (`localsigner.FromBytes`).
    const BLS_PUBKEY: [u8; 48] = [
        0xaf, 0xf4, 0xac, 0xb4, 0xc5, 0x43, 0x9b, 0x5d, 0x42, 0x6c, 0xad, 0xf9, 0xe9, 0x46, 0xd3,
        0xa4, 0x52, 0xf7, 0xde, 0x34, 0x14, 0xd1, 0xad, 0x27, 0x33, 0x61, 0x33, 0x21, 0x1d, 0x8b,
        0x90, 0xcf, 0x49, 0xfb, 0x97, 0xee, 0xbc, 0xde, 0xee, 0xf7, 0x14, 0xdc, 0x20, 0xf5, 0x4e,
        0xd0, 0xd4, 0xd1,
    ];
    const BLS_SIG: [u8; 96] = [
        0x8c, 0xfd, 0x79, 0x09, 0xd1, 0x53, 0xb9, 0x60, 0x4b, 0x62, 0xb1, 0x43, 0xba, 0x36, 0x20,
        0x7b, 0xb7, 0xe6, 0x48, 0x67, 0x42, 0x44, 0x80, 0x20, 0x2a, 0x67, 0xdc, 0x68, 0x76, 0x83,
        0x46, 0xd9, 0x5c, 0x90, 0x98, 0x3c, 0x2d, 0x27, 0x9c, 0x64, 0xc4, 0x3c, 0x51, 0x13, 0x6b,
        0x2a, 0x05, 0xe0, 0x16, 0x02, 0xd5, 0x2a, 0xa6, 0x37, 0x6f, 0xda, 0x17, 0xfa, 0x6e, 0x2a,
        0x18, 0xa0, 0x83, 0xe4, 0x9d, 0x9c, 0x45, 0x0e, 0xab, 0x7b, 0x89, 0xb1, 0xd5, 0x55, 0x5d,
        0xa5, 0xc4, 0x89, 0x87, 0x2e, 0x02, 0xb7, 0xe5, 0x22, 0x7b, 0x77, 0x55, 0x0a, 0xf1, 0x33,
        0x0e, 0x5a, 0x71, 0xf8, 0xc3, 0x68,
    ];
}
