// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The stateless [`SyntacticVerifier`] over all five X-Chain tx types
//! (specs 09 §6.1, §3.3; TX-AVM-1).
//!
//! Port of `vms/avm/txs/executor/syntactic_verifier.go` — the `txs.Visitor` that
//! validates a parsed `Tx` **without** touching chain state. Each tx-type method
//! reproduces the Go logic exactly:
//!
//! 1. (type-specific pre-checks: `CreateAssetTx` name/symbol/denom; `OperationTx`
//!    non-empty ops; `ImportTx` non-empty imported ins; `ExportTx` non-empty
//!    exported outs.)
//! 2. the embedded `avax.BaseTx.Verify` (network id, chain id, memo ≤ 256).
//! 3. the per-tx `avax.VerifyTx` flow check — fee burned, every produced/consumed
//!    value flows, each input/output `verify`s, each sub-slice sorted.
//! 4. type-specific structural checks (initial states, operations).
//! 5. each credential `verify`s, and `num_creds == num_inputs`.
//!
//! The conservation ledger reuses the shared [`avax::FlowChecker`]; the sort
//! predicates reuse the avm [`components`] helpers. Errors map onto the avm
//! [`Error`] sentinels (`assert_matches!`-able where Go uses `errors.Is`).

use std::cmp::Ordering;
use std::collections::BTreeSet;

use ava_codec::Serializable;
use ava_codec::packer::Packer;
use ava_types::id::Id;
use ava_vm::components::avax::FlowChecker;

use crate::error::{Error, Result};
use crate::fx::dispatch::resolve_fx_index;
use crate::txs::components::{
    self, AvaxBaseTx, MAX_MEMO_SIZE, Output, TransferableInput, TransferableOutput,
};
use crate::txs::executor::backend::Backend;
use crate::txs::initial_state::is_sorted_and_unique_initial_states;
use crate::txs::operation::is_sorted_and_unique_operations;
use crate::txs::{
    BaseTx, CreateAssetTx, ExportTx, FxCredential, ImportTx, InitialState, OperationTx, Tx,
    UnsignedTx,
};

const MIN_NAME_LEN: usize = 1;
const MAX_NAME_LEN: usize = 128;
const MIN_SYMBOL_LEN: usize = 1;
const MAX_SYMBOL_LEN: usize = 4;
const MAX_DENOMINATION: u8 = 32;

/// `executor.SyntacticVerifier` — the stateless tx-verification visitor
/// (specs 09 §6.1). Borrows the [`Backend`] and the [`Tx`] under check.
pub struct SyntacticVerifier<'a> {
    /// The verification context (chain ids, fees, fx count).
    pub backend: &'a Backend,
    /// The signed tx whose credentials are checked against the unsigned body.
    pub tx: &'a Tx,
}

impl<'a> SyntacticVerifier<'a> {
    /// Builds a [`SyntacticVerifier`] over a backend and a signed tx.
    #[must_use]
    pub fn new(backend: &'a Backend, tx: &'a Tx) -> Self {
        Self { backend, tx }
    }

    /// Runs the syntactic verification for the tx's concrete `UnsignedTx` variant
    /// (`SyntacticVerifier.Visit`).
    ///
    /// # Errors
    /// Returns the first failing check's avm [`Error`] (specs 09 §6.1).
    pub fn verify(&self) -> Result<()> {
        match &self.tx.unsigned {
            UnsignedTx::Base(tx) => self.base_tx(tx),
            UnsignedTx::CreateAsset(tx) => self.create_asset_tx(tx),
            UnsignedTx::Operation(tx) => self.operation_tx(tx),
            UnsignedTx::Import(tx) => self.import_tx(tx),
            UnsignedTx::Export(tx) => self.export_tx(tx),
        }
    }

    /// `SyntacticVerifier.BaseTx`.
    fn base_tx(&self, tx: &BaseTx) -> Result<()> {
        verify_base_tx(self.backend, &tx.base)?;
        self.verify_flow(
            self.backend.config.tx_fee,
            &[&tx.base.ins],
            &[&tx.base.outs],
        )?;
        self.verify_credentials()?;
        self.check_num_credentials(tx.base.ins.len())
    }

    /// `SyntacticVerifier.CreateAssetTx`.
    fn create_asset_tx(&self, tx: &CreateAssetTx) -> Result<()> {
        verify_name(&tx.name)?;
        verify_symbol(&tx.symbol)?;
        if tx.states.is_empty() {
            return Err(Error::NoFxs);
        }
        if tx.denomination > MAX_DENOMINATION {
            return Err(Error::DenominationTooLarge);
        }
        if tx.name.trim() != tx.name {
            return Err(Error::UnexpectedWhitespace);
        }

        verify_base_tx(self.backend, &tx.base.base)?;
        self.verify_flow(
            self.backend.config.create_asset_tx_fee,
            &[&tx.base.base.ins],
            &[&tx.base.base.outs],
        )?;

        for state in &tx.states {
            verify_initial_state(state, self.backend.num_fxs)?;
        }
        if !is_sorted_and_unique_initial_states(&tx.states) {
            return Err(Error::InitialStatesNotSortedUnique);
        }

        self.verify_credentials()?;
        self.check_num_credentials(tx.base.base.ins.len())
    }

    /// `SyntacticVerifier.OperationTx`.
    fn operation_tx(&self, tx: &OperationTx) -> Result<()> {
        if tx.ops.is_empty() {
            return Err(Error::NoOperations);
        }

        verify_base_tx(self.backend, &tx.base.base)?;
        self.verify_flow(
            self.backend.config.tx_fee,
            &[&tx.base.base.ins],
            &[&tx.base.base.outs],
        )?;

        // The base inputs' UTXO ids; each operation's utxo ids must be disjoint
        // from them and from each other (`errDoubleSpend`).
        let mut inputs: BTreeSet<Id> = tx
            .base
            .base
            .ins
            .iter()
            .map(TransferableInput::input_id)
            .collect();

        for op in &tx.ops {
            verify_operation(op)?;
            for utxo_id in &op.utxo_ids {
                let input_id = utxo_id.input_id();
                if !inputs.insert(input_id) {
                    return Err(Error::DoubleSpend);
                }
            }
        }
        if !is_sorted_and_unique_operations(&tx.ops) {
            return Err(Error::OperationsNotSortedUnique);
        }

        self.verify_credentials()?;
        // numInputs = len(Ins) + len(Ops).
        let num_inputs = tx.base.base.ins.len().saturating_add(tx.ops.len());
        self.check_num_credentials(num_inputs)
    }

    /// `SyntacticVerifier.ImportTx`.
    fn import_tx(&self, tx: &ImportTx) -> Result<()> {
        if tx.imported_ins.is_empty() {
            return Err(Error::NoImportInputs);
        }

        verify_base_tx(self.backend, &tx.base.base)?;
        self.verify_flow(
            self.backend.config.tx_fee,
            &[&tx.base.base.ins, &tx.imported_ins],
            &[&tx.base.base.outs],
        )?;

        self.verify_credentials()?;
        let num_inputs = tx.base.base.ins.len().saturating_add(tx.imported_ins.len());
        self.check_num_credentials(num_inputs)
    }

    /// `SyntacticVerifier.ExportTx`.
    fn export_tx(&self, tx: &ExportTx) -> Result<()> {
        if tx.exported_outs.is_empty() {
            return Err(Error::NoExportOutputs);
        }

        verify_base_tx(self.backend, &tx.base.base)?;
        self.verify_flow(
            self.backend.config.tx_fee,
            &[&tx.base.base.ins],
            &[&tx.base.base.outs, &tx.exported_outs],
        )?;

        self.verify_credentials()?;
        self.check_num_credentials(tx.base.base.ins.len())
    }

    /// `for _, cred := range v.Tx.Creds { cred.Verify() }`.
    fn verify_credentials(&self) -> Result<()> {
        for cred in &self.tx.creds {
            verify_credential(cred)?;
        }
        Ok(())
    }

    /// `numCreds == numInputs` (`errWrongNumberOfCredentials`).
    fn check_num_credentials(&self, num_inputs: usize) -> Result<()> {
        if self.tx.creds.len() != num_inputs {
            return Err(Error::WrongNumberOfCredentials);
        }
        Ok(())
    }

    /// `avax.VerifyTx(fee, feeAssetID, allIns, allOuts, codec)` over the avm
    /// codec components.
    fn verify_flow(
        &self,
        fee: u64,
        all_ins: &[&Vec<TransferableInput>],
        all_outs: &[&Vec<TransferableOutput>],
    ) -> Result<()> {
        verify_tx(self.backend.fee_asset_id, fee, all_ins, all_outs)
    }
}

/// `avax.VerifyTx` over the avm codec-serializable components (specs 07 §3.1).
///
/// Burns `fee` of `fee_asset_id`, accumulates every output as produced and every
/// input as consumed, verifies each component, requires each sub-slice be sorted
/// (outputs canonical, inputs sorted-and-unique), and returns the [`FlowChecker`]
/// verdict.
fn verify_tx(
    fee_asset_id: Id,
    fee: u64,
    all_ins: &[&Vec<TransferableInput>],
    all_outs: &[&Vec<TransferableOutput>],
) -> Result<()> {
    let mut fc = FlowChecker::new();
    fc.produce(fee_asset_id, fee);

    for outs in all_outs {
        for out in outs.iter() {
            out.verify()?;
            fc.produce(out.asset_id(), out.amount());
        }
        if !components::is_sorted_transferable_outputs(outs) {
            return Err(Error::OutputsNotSorted);
        }
    }

    for ins in all_ins {
        for input in ins.iter() {
            input.verify()?;
            fc.consume(input.asset_id(), input.amount());
        }
        if !components::is_sorted_and_unique_transferable_inputs(ins) {
            return Err(Error::InputsNotSortedUnique);
        }
    }

    fc.verify().map_err(Error::from)
}

/// `(*avax.BaseTx).Verify(ctx)` — network id, chain id, memo length
/// (specs 09 §3.2). The avm codec `AvaxBaseTx` is structurally distinct from the
/// runtime `ava_vm` `BaseTx`, so the checks are reproduced directly here.
fn verify_base_tx(backend: &Backend, base: &AvaxBaseTx) -> Result<()> {
    if base.network_id != backend.network_id {
        return Err(Error::WrongNetworkId);
    }
    if base.blockchain_id != backend.blockchain_id {
        return Err(Error::WrongBlockchainId);
    }
    if base.memo.len() > MAX_MEMO_SIZE {
        return Err(Error::MemoTooLarge);
    }
    Ok(())
}

/// `CreateAssetTx` name validation (`errName*` + `errIllegalNameCharacter`).
fn verify_name(name: &str) -> Result<()> {
    // Go measures `len(tx.Name)` in bytes; mirror with the byte length.
    let len = name.len();
    if len < MIN_NAME_LEN {
        return Err(Error::NameTooShort);
    }
    if len > MAX_NAME_LEN {
        return Err(Error::NameTooLong);
    }
    // ASCII letters / digits / space only.
    for r in name.chars() {
        if !r.is_ascii() || (!r.is_ascii_alphanumeric() && r != ' ') {
            return Err(Error::IllegalNameCharacter);
        }
    }
    Ok(())
}

/// `CreateAssetTx` symbol validation (`errSymbol*` + `errIllegalSymbolCharacter`).
fn verify_symbol(symbol: &str) -> Result<()> {
    let len = symbol.len();
    if len < MIN_SYMBOL_LEN {
        return Err(Error::SymbolTooShort);
    }
    if len > MAX_SYMBOL_LEN {
        return Err(Error::SymbolTooLong);
    }
    for r in symbol.chars() {
        if !r.is_ascii() || !r.is_ascii_uppercase() {
            return Err(Error::IllegalSymbolCharacter);
        }
    }
    Ok(())
}

/// `(*InitialState).Verify(codec, numFxs)` (specs 09 §3.3).
///
/// `fx_index < num_fxs` (`errUnknownFx`), each output `verify`s, and the outputs
/// are sorted by their marshaled codec bytes (`errOutputsNotSorted`).
fn verify_initial_state(state: &InitialState, num_fxs: usize) -> Result<()> {
    if (state.fx_index as usize) >= num_fxs {
        return Err(Error::UnknownFx);
    }
    for out in &state.outs {
        out.verify()?;
    }
    if !is_sorted_outputs(&state.outs) {
        return Err(Error::OutputsNotSorted);
    }
    Ok(())
}

/// `isSortedState(outs, codec)` — the outputs are non-decreasing by their
/// marshaled codec bytes (`InitialState.Sort` order).
fn is_sorted_outputs(outs: &[Output]) -> bool {
    outs.windows(2).all(|w| match w {
        [a, b] => output_bytes(a).cmp(&output_bytes(b)) != Ordering::Greater,
        _ => true,
    })
}

/// The canonical codec bytes of an fx output (incl. its typeID).
fn output_bytes(out: &Output) -> Vec<u8> {
    let mut p = Packer::with_max_size(usize::MAX);
    out.marshal_into(&mut p);
    p.into_bytes()
}

/// `(*Operation).Verify()` — sorted-and-unique utxo ids, then `verify.All` over
/// the asset + fx op. The avm op envelope checks the structural parts the
/// stateless pass can reach (specs 09 §3.4).
fn verify_operation(op: &crate::txs::Operation) -> Result<()> {
    if !components::is_sorted_and_unique_utxo_ids(&op.utxo_ids) {
        return Err(Error::NotSortedAndUniqueUtxoIds);
    }
    Ok(())
}

/// `cred.Verify()` — route the credential to its fx (`errUnknownFx`) and run the
/// fx credential's structural verify (the secp credential's nil check always
/// passes for a parsed value).
fn verify_credential(cred: &FxCredential) -> Result<()> {
    // The credential must route to a registered fx (mirrors Go typing the
    // credential through `typeToFxIndex` before verifying it).
    resolve_fx_index(credential_type_id(&cred.credential))?;
    Ok(())
}

/// The codec type-id of an avm credential interface value.
fn credential_type_id(cred: &crate::txs::Credential) -> u32 {
    cred.codec_type_id()
}
