// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `executor.AtomicTx` — the legacy pre-Banff atomic-tx executor
//! (`vms/platformvm/txs/executor/atomic_tx_executor.go`, specs 08 §2.4).
//!
//! [`AtomicTxExecutor`] is the [`Visitor`](crate::txs::Visitor) used to execute
//! an [`ImportTx`] / [`ExportTx`] that lives inside an `ApricotAtomicBlock`. Post
//! AP5/Banff the import/export txs became ordinary decision txs handled by the
//! M4.16 [`StandardTxExecutor`](super::StandardTxExecutor) inside a
//! `BanffStandardBlock`; this executor is the *legacy* atomic-block path only.
//!
//! Its job is the shared-memory flow that M4.16 deferred (ATOMIC-1, specs 00
//! §11.1.7):
//!
//! - **Import:** the imported inputs reference UTXOs that live in the *source
//!   chain's* shared memory, not the local UTXO set. This executor fetches those
//!   UTXOs from a [`SharedMemory`] handle, combines their amounts with the local
//!   inputs, and runs the value-conservation flow check across the combined set
//!   (`sum(local ins) + sum(imported ins) == sum(outs) + fee`). The consumed
//!   UTXO ids are recorded as the `RemoveRequests` against the source chain.
//! - **Export:** the exported outputs are produced into the *destination chain's*
//!   shared memory, not the local UTXO set. The local flow check balances the
//!   local inputs against the local outputs plus the exported outputs plus the
//!   fee, and the exported `avax.UTXO`s are marshaled into the `PutRequests`
//!   against the destination chain.
//!
//! The result mirrors Go's `AtomicTx` return: the `(inputs, atomicRequests,
//! onAccept)` triple, reusing M4.16's [`StandardTxOutputs`] /
//! [`AtomicRequests`](super::AtomicRequests) so the block executor applies the
//! shared-memory ops identically regardless of which block type carried the tx.
//!
//! Like the Go `atomicTxExecutor`, every non-import/export tx type is rejected as
//! [`Error::WrongTxType`] via the default [`Visitor`] impls.

use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::state::diff::Diff;
use crate::txs::components::{TransferableInput, TransferableOutput};
use crate::txs::{ExportTx, ImportTx, Visitor};
use crate::utxo::{self, Utxo};

use super::backend::Backend;
use super::state_changes;
use super::{AtomicRequests, StandardTxOutputs};

/// A read handle over the cross-chain shared-memory store (`atomic.SharedMemory`,
/// specs 00 §11.1.7).
///
/// The atomic import path needs to read the source chain's exported UTXOs to
/// verify the import flow. The node-wide `chains/atomic` shared-memory service is
/// not yet ported (it belongs to the chain manager, M4.20+), so this trait is the
/// minimal seam the executor depends on: given a peer chain id and a list of UTXO
/// id keys, return the stored UTXO bytes in the same order.
///
/// Production wiring will supply the real shared-memory database behind this
/// trait; the tests supply an in-memory map.
pub trait SharedMemory {
    /// `SharedMemory.Get(peerChainID, keys)` — fetch the value blobs stored under
    /// `keys` for the `(this chain, peer_chain)` link, in `keys` order.
    ///
    /// # Errors
    /// Returns an error if any key is absent (mirroring Go's `SharedMemory.Get`,
    /// which fails the whole batch if a key is missing).
    fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>>;
}

/// `atomicTxExecutor` — a [`Visitor`] that executes a legacy atomic
/// (`ApricotAtomicBlock`) import/export tx against a [`Diff`].
///
/// Construct with [`AtomicTxExecutor::new`], dispatch via
/// [`UnsignedTx::visit`](crate::txs::UnsignedTx::visit), then take
/// [`AtomicTxExecutor::into_outputs`].
pub struct AtomicTxExecutor<'a> {
    backend: &'a Backend,
    state: &'a mut Diff,
    /// The cross-chain shared-memory read handle (import flow lookups).
    shared_memory: &'a dyn SharedMemory,
    /// The tx id (`sha256(signed_bytes)`). Derived from the signed tx in
    /// [`AtomicTxExecutor::new`]; the produced/exported UTXOs are keyed by it.
    ///
    /// The full signed tx (credentials) is not retained: this legacy atomic path
    /// performs the single-asset value-conservation flow check, not the
    /// per-credential `VerifySpendUTXOs`. The credential-verifying flow checker is
    /// layered on alongside the standard executor's as it matures.
    tx_id: Id,
    /// Accumulated outputs.
    outputs: StandardTxOutputs,
}

impl<'a> AtomicTxExecutor<'a> {
    /// Builds an executor over `state` for the signed `tx`, reading imported
    /// UTXOs through `shared_memory`.
    pub fn new(
        backend: &'a Backend,
        state: &'a mut Diff,
        shared_memory: &'a dyn SharedMemory,
        tx: &'a crate::txs::Tx,
    ) -> Self {
        Self {
            backend,
            state,
            shared_memory,
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

    /// `avax.Consume` + `avax.Produce` over the embedded base tx (the local UTXO
    /// set; the imported inputs / exported outputs hit shared memory instead).
    fn consume_produce(
        &mut self,
        ins: &[TransferableInput],
        outs: &[TransferableOutput],
    ) -> Result<()> {
        utxo::consume(self.state, ins);
        utxo::produce(self.state, self.tx_id, outs)
    }
}

impl Visitor for AtomicTxExecutor<'_> {
    type Error = Error;

    fn import(&mut self, tx: &ImportTx) -> Result<()> {
        tx.base.syntactic_verify()?;

        // Record the imported UTXO ids: they are the consumed shared-memory
        // inputs and the source chain's `RemoveRequests`.
        let mut utxo_id_keys = Vec::with_capacity(tx.imported_inputs.len());
        for input in &tx.imported_inputs {
            let utxo_id = input.input_id();
            self.outputs.inputs.insert(utxo_id);
            utxo_id_keys.push(utxo_id.to_bytes().to_vec());
        }

        // Skip the shared-memory flow check until bootstrapped, mirroring Go
        // (`e.backend.Bootstrapped.Get()`): pre-bootstrap the peer chains are not
        // guaranteed up-to-date, so the imported UTXOs may not yet be visible.
        if self.backend.bootstrapped {
            // Fetch the imported UTXOs from the source chain's shared memory and
            // sum their amounts (the consumed value that lives off-chain).
            let imported_bytes = self.shared_memory.get(tx.source_chain, &utxo_id_keys)?;
            let mut imported_consumed: u64 = 0;
            for (utxo_bytes, input) in imported_bytes.iter().zip(tx.imported_inputs.iter()) {
                let imported = Utxo::unmarshal(utxo_bytes)?;
                // The imported UTXO must hold the asset the input claims to spend.
                if imported.asset_id != input.asset_id {
                    return Err(Error::InvalidComponent);
                }
                // Only AVAX contributes to the (single-asset) flow balance; a
                // non-AVAX import therefore cannot fund the AVAX outputs + fee,
                // matching the multi-asset `VerifySpendUTXOs` semantics for the
                // AVAX slice.
                if input.asset_id == self.backend.avax_asset_id {
                    imported_consumed = imported_consumed
                        .checked_add(input.amount())
                        .ok_or(Error::Overflow)?;
                }
            }

            // Local inputs are verified against the on-chain UTXO set, and the
            // combined (local + imported) consumed value must cover the outputs
            // plus the fee: `local_in + imported_in == out + fee`.
            let fee = self.fee()?;
            verify_atomic_spend(
                self.state,
                &tx.base.base.ins,
                imported_consumed,
                &tx.base.base.outs,
                &[],
                fee,
                self.backend.avax_asset_id,
            )?;
        }

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)?;

        // Note: apply the atomic request even when the flow check is skipped, so
        // the shared state is correct if verification is later enabled (Go does
        // the same).
        self.outputs.atomic_requests.insert(
            tx.source_chain,
            AtomicRequests {
                put_requests: Vec::new(),
                remove_requests: utxo_id_keys,
            },
        );
        Ok(())
    }

    fn export(&mut self, tx: &ExportTx) -> Result<()> {
        tx.base.syntactic_verify()?;

        // The exported outputs leave for the destination chain's shared memory;
        // they count as produced value in the flow check but are not added to the
        // local UTXO set. Balance `local_in == local_out + exported_out + fee`.
        if self.backend.bootstrapped {
            let fee = self.fee()?;
            verify_atomic_spend(
                self.state,
                &tx.base.base.ins,
                0,
                &tx.base.base.outs,
                &tx.exported_outputs,
                fee,
                self.backend.avax_asset_id,
            )?;
        }

        let (ins, outs) = (tx.base.base.ins.clone(), tx.base.base.outs.clone());
        self.consume_produce(&ins, &outs)?;

        // Build the destination chain's `PutRequests` from the exported outputs.
        // Their UTXOIDs continue the local output index space (`len(Outs) + i`),
        // matching Go so the produced `avax.UTXO` ids are deterministic.
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

/// The atomic value-conservation check: `sum(local_ins) + imported_consumed ==
/// sum(local_outs) + sum(off_chain_outs) + fee`, single AVAX asset.
///
/// `local_ins` are verified against the on-chain UTXO set (each must exist and
/// match `avax_asset_id`); `imported_consumed` is the pre-summed amount of the
/// imported inputs whose UTXOs live in shared memory; `off_chain_outs` are the
/// exported outputs that leave for another chain's shared memory (they are not
/// produced locally but still count as produced value). This is the single-asset
/// slice of Go's `VerifySpendUTXOs` over the combined (on-chain + shared-memory)
/// UTXO set (specs 08 §2.4, ATOMIC-1).
///
/// # Errors
/// - [`Error::Database`] if a local input's UTXO is absent.
/// - [`Error::Codec`] if a local input's UTXO cannot be decoded.
/// - [`Error::InvalidComponent`] if an asset id mismatches.
/// - [`Error::FlowCheckFailed`] if the spend does not balance.
/// - [`Error::Overflow`] if any sum overflows `u64`.
fn verify_atomic_spend(
    chain: &dyn Chain,
    local_ins: &[TransferableInput],
    imported_consumed: u64,
    local_outs: &[TransferableOutput],
    off_chain_outs: &[TransferableOutput],
    fee: u64,
    avax_asset_id: Id,
) -> Result<()> {
    let mut consumed: u64 = imported_consumed;
    for input in local_ins {
        if input.asset_id != avax_asset_id {
            return Err(Error::InvalidComponent);
        }
        let bytes = chain.get_utxo(input.input_id())?;
        let utxo = Utxo::unmarshal(&bytes)?;
        if utxo.asset_id != avax_asset_id {
            return Err(Error::InvalidComponent);
        }
        consumed = consumed
            .checked_add(input.amount())
            .ok_or(Error::Overflow)?;
    }

    let mut produced: u64 = fee;
    for out in local_outs.iter().chain(off_chain_outs.iter()) {
        if out.asset_id != avax_asset_id {
            return Err(Error::InvalidComponent);
        }
        produced = produced.checked_add(out.amount()).ok_or(Error::Overflow)?;
    }

    if produced != consumed {
        return Err(Error::FlowCheckFailed);
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing
)]
mod atomic_executor {
    //! M4.18 conformance tests — the legacy `ApricotAtomicBlock` import/export
    //! path, ported from Go `atomic_tx_executor_test.go` / `import_test.go` /
    //! `export_test.go`.
    //!
    //! Each test builds a `State`-backed `Diff`, a small in-memory shared-memory
    //! double, runs the executor, and asserts the resulting `(consumed inputs,
    //! atomic requests, Diff mutations)`.

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use ava_database::MemDb;
    use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
    use ava_types::id::Id;
    use ava_types::node_id::NodeId;
    use ava_types::short_id::ShortId;
    use ava_utils::clock::MockClock;

    use super::*;
    use crate::state::chain::{Chain, Versions};
    use crate::state::state::State;
    use crate::txs::components::{
        BaseTx as AvaxBaseTx, Input, Output, TransferableInput, TransferableOutput,
    };
    use crate::txs::executor::backend::{StakingConfig, UpgradeSchedule};
    use crate::txs::{AdvanceTimeTx, Tx, UnsignedTx};

    const AVAX_ASSET: [u8; 32] = [0x42; 32];
    const OTHER_ASSET: [u8; 32] = [0x99; 32];
    const AVAX: u64 = 1_000_000_000;
    /// The mainnet static tx fee (`MilliAvax`).
    const TX_FEE: u64 = 1_000_000;
    /// The X-Chain id used as the peer chain in these fixtures.
    const X_CHAIN: [u8; 32] = [0x58; 32];

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

    /// An in-memory `chains/atomic` shared-memory double: maps a peer chain id +
    /// UTXO id key to the stored UTXO bytes.
    #[derive(Default)]
    struct MapSharedMemory {
        entries: BTreeMap<(Id, Vec<u8>), Vec<u8>>,
    }

    impl MapSharedMemory {
        /// Stores `utxo` under the link to `peer_chain` (the source chain holding
        /// the exported UTXO).
        fn put(&mut self, peer_chain: Id, utxo: &Utxo) {
            self.entries.insert(
                (peer_chain, utxo.input_id().to_bytes().to_vec()),
                utxo.marshal().expect("marshal shared-memory utxo"),
            );
        }
    }

    impl SharedMemory for MapSharedMemory {
        fn get(&self, peer_chain: Id, keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>> {
            keys.iter()
                .map(|key| {
                    self.entries
                        .get(&(peer_chain, key.clone()))
                        .cloned()
                        .ok_or(Error::InvalidComponent)
                })
                .collect()
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
    /// static fees, and the given bootstrapped flag.
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
        asset_input(Id::from(AVAX_ASSET), tx_id, index, amt)
    }

    /// A `TransferableInput` of `asset` consuming `(tx_id, index, amt)`.
    fn asset_input(asset: Id, tx_id: Id, index: u32, amt: u64) -> TransferableInput {
        TransferableInput {
            tx_id,
            output_index: index,
            asset_id: asset,
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

    /// The `avax.UTXO` referenced by `input`, holding `amt` of `input.asset_id`.
    fn utxo_for(input: &TransferableInput, amt: u64) -> Utxo {
        Utxo {
            tx_id: input.tx_id,
            output_index: input.output_index,
            asset_id: input.asset_id,
            out: Output::Transfer(TransferOutput::new(amt, owners(1))),
        }
    }

    fn run(
        backend: &Backend,
        diff: &mut Diff,
        sm: &dyn SharedMemory,
        unsigned: UnsignedTx,
    ) -> Result<StandardTxOutputs> {
        let mut tx = Tx::new(unsigned);
        tx.initialize(crate::txs::codec::Codec()).expect("init");
        let mut exec = AtomicTxExecutor::new(backend, diff, sm, &tx);
        tx.unsigned.visit(&mut exec)?;
        Ok(exec.into_outputs())
    }

    /// Runs `unsigned` and returns the error, asserting the run failed.
    fn run_err(
        backend: &Backend,
        diff: &mut Diff,
        sm: &dyn SharedMemory,
        unsigned: UnsignedTx,
    ) -> Error {
        match run(backend, diff, sm, unsigned) {
            Ok(_) => panic!("expected the tx to fail execution"),
            Err(e) => e,
        }
    }

    /// `AtomicTxExecutor` rejects a non-import/export tx (`AdvanceTimeTx`) with
    /// the wrong-tx-type sentinel (Go `ErrWrongTxType`).
    #[test]
    fn atomic_executor_rejects_non_atomic_tx() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let sm = MapSharedMemory::default();
        let b = backend(UpgradeSchedule::all_active(), true);
        let err = run_err(
            &b,
            &mut diff,
            &sm,
            UnsignedTx::AdvanceTime(AdvanceTimeTx::default()),
        );
        assert!(matches!(err, Error::WrongTxType));
    }

    /// Valid `ImportTx`: an imported AVAX UTXO from the X-Chain pays the fee. The
    /// imported UTXO id is recorded as a consumed input and a `RemoveRequest`
    /// against the source chain; no local UTXO is produced or consumed.
    #[test]
    fn atomic_executor_import_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);

        // The imported input references a UTXO sitting in the X-Chain's shared
        // memory worth exactly the fee + the produced output.
        let imported_amt = 10 * AVAX;
        let imported_in = avax_input(Id::from([0x11; 32]), 0, imported_amt);
        let mut sm = MapSharedMemory::default();
        sm.put(x_chain, &utxo_for(&imported_in, imported_amt));

        let tx = ImportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(imported_amt - TX_FEE)],
                ins: vec![],
                memo: vec![],
            }),
            source_chain: x_chain,
            imported_inputs: vec![imported_in.clone()],
        };
        let out = run(&b, &mut diff, &sm, UnsignedTx::Import(tx)).expect("import");

        // The imported UTXO id is the consumed shared-memory input.
        let utxo_id = imported_in.input_id();
        assert_eq!(out.inputs.len(), 1);
        assert!(out.inputs.contains(&utxo_id));
        // A single RemoveRequest against the source chain, no PutRequests.
        let reqs = out.atomic_requests.get(&x_chain).expect("source requests");
        assert_eq!(reqs.remove_requests, vec![utxo_id.to_bytes().to_vec()]);
        assert!(reqs.put_requests.is_empty());
    }

    /// `ImportTx` whose imported value does not cover the outputs + fee fails the
    /// atomic flow check.
    #[test]
    fn atomic_executor_import_insufficient_funds() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);

        let imported_amt = 10 * AVAX;
        let imported_in = avax_input(Id::from([0x12; 32]), 0, imported_amt);
        let mut sm = MapSharedMemory::default();
        sm.put(x_chain, &utxo_for(&imported_in, imported_amt));

        // Output equals the full imported amount, leaving nothing for the fee.
        let tx = ImportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(imported_amt)],
                ins: vec![],
                memo: vec![],
            }),
            source_chain: x_chain,
            imported_inputs: vec![imported_in],
        };
        let err = run_err(&b, &mut diff, &sm, UnsignedTx::Import(tx));
        assert!(matches!(err, Error::FlowCheckFailed), "got {err:?}");
    }

    /// `ImportTx` referencing a UTXO absent from the source chain's shared memory
    /// fails (Go's `SharedMemory.Get` errors on a missing key).
    #[test]
    fn atomic_executor_import_missing_shared_memory_utxo() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);

        // No UTXO is funded in shared memory.
        let sm = MapSharedMemory::default();
        let imported_in = avax_input(Id::from([0x13; 32]), 0, 10 * AVAX);
        let tx = ImportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![],
                memo: vec![],
            }),
            source_chain: x_chain,
            imported_inputs: vec![imported_in],
        };
        let err = run_err(&b, &mut diff, &sm, UnsignedTx::Import(tx));
        assert!(matches!(err, Error::InvalidComponent), "got {err:?}");
    }

    /// Un-bootstrapped: the shared-memory flow check is skipped, so an import that
    /// references an unfunded shared-memory UTXO still succeeds (the request is
    /// recorded for later application, matching Go).
    #[test]
    fn atomic_executor_import_skips_flow_check_unbootstrapped() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), false);
        let x_chain = Id::from(X_CHAIN);

        let sm = MapSharedMemory::default(); // intentionally empty
        let imported_in = avax_input(Id::from([0x14; 32]), 0, 10 * AVAX);
        let utxo_id = imported_in.input_id();
        let tx = ImportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![],
                memo: vec![],
            }),
            source_chain: x_chain,
            imported_inputs: vec![imported_in],
        };
        let out = run(&b, &mut diff, &sm, UnsignedTx::Import(tx)).expect("import (unbootstrapped)");
        assert!(out.inputs.contains(&utxo_id));
        let reqs = out.atomic_requests.get(&x_chain).expect("source requests");
        assert_eq!(reqs.remove_requests, vec![utxo_id.to_bytes().to_vec()]);
    }

    /// `ImportTx` whose imported UTXO holds a different asset than the input
    /// claims (the input asks for a non-AVAX asset) cannot cover the AVAX output
    /// + fee, so the flow check fails.
    #[test]
    fn atomic_executor_import_wrong_asset() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut diff = diff_with_utxos(ts, &[]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);

        // The input claims a non-AVAX asset; the executor balances on AVAX only.
        let imported_in = asset_input(Id::from(OTHER_ASSET), Id::from([0x15; 32]), 0, 10 * AVAX);
        let mut sm = MapSharedMemory::default();
        sm.put(x_chain, &utxo_for(&imported_in, 10 * AVAX));

        let tx = ImportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![avax_output(10 * AVAX - TX_FEE)],
                ins: vec![],
                memo: vec![],
            }),
            source_chain: x_chain,
            imported_inputs: vec![imported_in],
        };
        let err = run_err(&b, &mut diff, &sm, UnsignedTx::Import(tx));
        // The AVAX output cannot be covered by a non-AVAX import.
        assert!(matches!(err, Error::FlowCheckFailed), "got {err:?}");
    }

    /// Valid `ExportTx`: a local AVAX UTXO funds an exported output to the
    /// X-Chain. The exported UTXO is recorded as a `PutRequest` against the
    /// destination chain; the local funding UTXO is consumed.
    #[test]
    fn atomic_executor_export_valid() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x21; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 10 * AVAX)]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);
        let sm = MapSharedMemory::default();

        // Spend 10 AVAX: export (10 - fee), no local change output.
        let exported = avax_output(10 * AVAX - TX_FEE);
        let tx = ExportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![avax_input(fund, 0, 10 * AVAX)],
                memo: vec![],
            }),
            destination_chain: x_chain,
            exported_outputs: vec![exported.clone()],
        };
        let out = run(&b, &mut diff, &sm, UnsignedTx::Export(tx)).expect("export");

        // No consumed shared-memory inputs; one PutRequest to the destination.
        assert!(out.inputs.is_empty());
        let reqs = out.atomic_requests.get(&x_chain).expect("dest requests");
        assert!(reqs.remove_requests.is_empty());
        assert_eq!(reqs.put_requests.len(), 1);

        // The PutRequest key/value is the exported UTXO at index `len(outs) + 0`.
        let tx_id = {
            let mut tx2 = Tx::new(UnsignedTx::Export(ExportTx {
                base: crate::txs::BaseTx::new(AvaxBaseTx {
                    network_id: 1,
                    blockchain_id: Id::EMPTY,
                    outs: vec![],
                    ins: vec![avax_input(fund, 0, 10 * AVAX)],
                    memo: vec![],
                }),
                destination_chain: x_chain,
                exported_outputs: vec![exported.clone()],
            }));
            tx2.initialize(crate::txs::codec::Codec()).expect("init");
            tx2.id()
        };
        let expected = exported_utxo(tx_id, 0, &exported);
        assert_eq!(
            reqs.put_requests[0].0,
            expected.input_id().to_bytes().to_vec()
        );
        assert_eq!(reqs.put_requests[0].1, expected.marshal().unwrap());

        // The local funding UTXO is consumed; no local UTXO produced.
        assert!(diff.get_utxo(avax_input(fund, 0, 0).input_id()).is_err());
    }

    /// `ExportTx` that does not balance (exports more than it funds) fails the
    /// flow check.
    #[test]
    fn atomic_executor_export_insufficient_funds() {
        let ts = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let fund = Id::from([0x22; 32]);
        let mut diff = diff_with_utxos(ts, &[(fund, 0, 10 * AVAX)]);
        let b = backend(UpgradeSchedule::durango_only(), true);
        let x_chain = Id::from(X_CHAIN);
        let sm = MapSharedMemory::default();

        // Export the full funded amount, leaving nothing for the fee.
        let tx = ExportTx {
            base: crate::txs::BaseTx::new(AvaxBaseTx {
                network_id: 1,
                blockchain_id: Id::EMPTY,
                outs: vec![],
                ins: vec![avax_input(fund, 0, 10 * AVAX)],
                memo: vec![],
            }),
            destination_chain: x_chain,
            exported_outputs: vec![avax_output(10 * AVAX)],
        };
        let err = run_err(&b, &mut diff, &sm, UnsignedTx::Export(tx));
        assert!(matches!(err, Error::FlowCheckFailed), "got {err:?}");
    }
}
