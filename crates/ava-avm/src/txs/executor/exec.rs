// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Executor` — applies a verified X-Chain tx to a [`Chain`] state diff
//! (specs 09 §6.3, EXEC-AVM-1, ATOMIC-1).
//!
//! Port of `vms/avm/txs/executor/executor.go` (the `txs.Visitor` that mutates
//! state after the [`SyntacticVerifier`](super::syntactic::SyntacticVerifier)
//! and [`SemanticVerifier`](super::semantic::SemanticVerifier) passes). The
//! executor is the third and final visitor in the pipeline; it applies a fully
//! verified tx to a [`Chain`] diff, recording atomic requests for the block
//! executor to apply on accept.
//!
//! ## EXEC-AVM-1 — output index assignment
//!
//! Output indices are assigned **monotonically** in this order:
//!
//! 1. `BaseTx.outs` → indices `0 .. len(outs)`.
//! 2. `CreateAssetTx.states[*].outs` → continuing from `len(outs)` across
//!    all `InitialState`s in declared order (by `fx_index`).
//! 3. `OperationTx.ops[*].outs()` → continuing from `len(outs)`.
//! 4. `ExportTx.exported_outs` → continuing from `len(outs)`.
//!
//! Any deviation changes UTXO IDs and diverges from the Go reference node.
//!
//! ## ATOMIC-1 — export UTXO bytes
//!
//! Exported UTXOs are marshaled through the **avm codec v0**
//! ([`crate::txs::codec::Codec`]) so the byte layout matches `avax.UTXO`
//! decoded by the peer chain.
//!
//! ## Atomic requests
//!
//! Import: `Requests { remove: [input_id ..] }` keyed by `source_chain`.
//! Export: `Requests { put: [Element{key=input_id, value=marshal(utxo),
//! traits=addresses}] }` keyed by `destination_chain`.
//! All requests are keyed in a `BTreeMap` for deterministic ordering (00 §6.1).

use std::collections::BTreeMap;

use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, Requests};

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::txs::components::{Output, TransferableOutput};
use crate::txs::executor::semantic::Utxo;
use crate::txs::{BaseTx, CreateAssetTx, ExportTx, ImportTx, OperationTx, UnsignedTx};

/// The outputs of a successful [`Executor::execute`] run.
///
/// Mirrors Go `executor.go`'s return: the consumed shared-memory input ids and
/// the per-chain atomic `Requests` to apply on block accept.
#[derive(Debug, Default)]
pub struct ExecutorOutputs {
    /// The UTXO ids consumed from shared memory (import txs only).
    pub inputs: std::collections::BTreeSet<Id>,
    /// The per-chain atomic put/remove ops to apply on accept.
    pub atomic_requests: BTreeMap<Id, Requests>,
}

/// `executor.Executor` — applies a verified `UnsignedTx` to a `Chain` diff.
pub struct Executor;

impl Executor {
    /// Executes `unsigned` against `state`, applying all UTXO state mutations
    /// in-place and returning the [`ExecutorOutputs`] for block-accept wiring.
    ///
    /// # Errors
    /// Returns an [`Error`] if marshaling fails (e.g. codec error on export
    /// UTXO serialization) or an index conversion overflows.
    pub fn execute(
        unsigned: &UnsignedTx,
        tx_id: Id,
        state: &mut dyn Chain,
    ) -> Result<ExecutorOutputs> {
        let mut outputs = ExecutorOutputs::default();
        match unsigned {
            UnsignedTx::Base(tx) => {
                Self::execute_base(tx, tx_id, state)?;
            }
            UnsignedTx::CreateAsset(tx) => {
                Self::execute_create_asset(tx, tx_id, state)?;
            }
            UnsignedTx::Operation(tx) => {
                Self::execute_operation(tx, tx_id, state)?;
            }
            UnsignedTx::Import(tx) => {
                Self::execute_import(tx, tx_id, state, &mut outputs)?;
            }
            UnsignedTx::Export(tx) => {
                Self::execute_export(tx, tx_id, state, &mut outputs)?;
            }
        }
        Ok(outputs)
    }

    // -----------------------------------------------------------------------
    // BaseTx: consume inputs, produce outputs at indices 0..len(outs)
    // -----------------------------------------------------------------------

    fn execute_base(tx: &BaseTx, tx_id: Id, state: &mut dyn Chain) -> Result<()> {
        consume(state, &tx.base.ins);
        produce(state, tx_id, &tx.base.outs)
    }

    // -----------------------------------------------------------------------
    // CreateAssetTx: BaseTx then InitialState outputs (EXEC-AVM-1)
    // -----------------------------------------------------------------------

    fn execute_create_asset(tx: &CreateAssetTx, tx_id: Id, state: &mut dyn Chain) -> Result<()> {
        // Consume + produce the embedded BaseTx (indices 0..len(outs)).
        consume(state, &tx.base.base.ins);
        produce(state, tx_id, &tx.base.base.outs)?;

        // Per-InitialState outputs: output_index continues from len(outs).
        let base_len = u32::try_from(tx.base.base.outs.len()).map_err(|_| Error::SpendOverflow)?;
        let mut next_index = base_len;

        for initial_state in &tx.states {
            for out in &initial_state.outs {
                // asset_id = tx_id (the CreateAssetTx's id IS the asset id).
                let utxo = Utxo {
                    tx_id,
                    output_index: next_index,
                    asset_id: tx_id,
                    out: out.clone(),
                };
                let utxo_id = utxo.input_id();
                let bytes = utxo.marshal()?;
                state.add_utxo(utxo_id, bytes);
                next_index = next_index.checked_add(1).ok_or(Error::SpendOverflow)?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // OperationTx: BaseTx + delete op inputs (op output production gated on M5.7/M5.8)
    // -----------------------------------------------------------------------

    fn execute_operation(tx: &OperationTx, tx_id: Id, state: &mut dyn Chain) -> Result<()> {
        // BaseTx part (indices 0..len(outs)).
        consume(state, &tx.base.base.ins);
        produce(state, tx_id, &tx.base.base.outs)?;

        // Per-operation: delete each consumed UTXO (op inputs are already in
        // local state — the semantic verifier confirmed them). Operations produce
        // outputs whose type depends on the fx; the placeholder `FxOperation`
        // carries no typed outputs until M5.7/M5.8, so we only delete inputs here.
        // The output_index counter continues from len(outs) for when op outputs land.
        for op in &tx.ops {
            // Delete each UTXO referenced by this operation.
            for utxo_id_ref in &op.utxo_ids {
                state.delete_utxo(utxo_id_ref.input_id());
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ImportTx: BaseTx + record imported input_ids + Requests{remove}
    // -----------------------------------------------------------------------

    fn execute_import(
        tx: &ImportTx,
        tx_id: Id,
        state: &mut dyn Chain,
        outputs: &mut ExecutorOutputs,
    ) -> Result<()> {
        // Consume local base inputs + produce base outputs.
        consume(state, &tx.base.base.ins);
        produce(state, tx_id, &tx.base.base.outs)?;

        // Build the remove-request for the source chain.
        let mut remove = Vec::with_capacity(tx.imported_ins.len());
        for input in &tx.imported_ins {
            let input_id = input.input_id();
            outputs.inputs.insert(input_id);
            remove.push(input_id.to_bytes().to_vec());
        }

        if !remove.is_empty() {
            outputs.atomic_requests.insert(
                tx.source_chain,
                Requests {
                    remove,
                    put: Vec::new(),
                },
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // ExportTx: BaseTx + Requests{put} for each exported out (EXEC-AVM-1, ATOMIC-1)
    // -----------------------------------------------------------------------

    fn execute_export(
        tx: &ExportTx,
        tx_id: Id,
        state: &mut dyn Chain,
        outputs: &mut ExecutorOutputs,
    ) -> Result<()> {
        // Consume local base inputs + produce base outputs (indices 0..len(outs)).
        consume(state, &tx.base.base.ins);
        produce(state, tx_id, &tx.base.base.outs)?;

        // Exported outputs: output_index continues from len(base.outs) (EXEC-AVM-1).
        let base_len = u32::try_from(tx.base.base.outs.len()).map_err(|_| Error::SpendOverflow)?;

        let mut put = Vec::with_capacity(tx.exported_outs.len());
        for (i, out) in tx.exported_outs.iter().enumerate() {
            let i_u32 = u32::try_from(i).map_err(|_| Error::SpendOverflow)?;
            let output_index = base_len.checked_add(i_u32).ok_or(Error::SpendOverflow)?;

            // Build the exported UTXO (asset_id from the transferable output).
            let utxo = Utxo {
                tx_id,
                output_index,
                asset_id: out.asset_id(),
                out: out.out.clone(),
            };
            let key = utxo.input_id().to_bytes().to_vec();
            let value = utxo.marshal()?;
            let traits = output_addresses(&out.out);

            put.push(Element { key, value, traits });
        }

        if !put.is_empty() {
            outputs.atomic_requests.insert(
                tx.destination_chain,
                Requests {
                    remove: Vec::new(),
                    put,
                },
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers: consume / produce / output_addresses
// ---------------------------------------------------------------------------

/// `avax.consume` — delete each input's UTXO from state.
fn consume(state: &mut dyn Chain, ins: &[crate::txs::components::TransferableInput]) {
    for input in ins {
        state.delete_utxo(input.input_id());
    }
}

/// `avax.produce` — add a UTXO for each output at `output_index = i`
/// (`tx_id, asset_id = out.asset_id()`).
///
/// # Errors
/// Returns [`Error::Codec`] if marshaling any UTXO fails.
fn produce(state: &mut dyn Chain, tx_id: Id, outs: &[TransferableOutput]) -> Result<()> {
    for (i, out) in outs.iter().enumerate() {
        let output_index = u32::try_from(i).map_err(|_| Error::SpendOverflow)?;
        let utxo = Utxo {
            tx_id,
            output_index,
            asset_id: out.asset_id(),
            out: out.out.clone(),
        };
        let utxo_id = utxo.input_id();
        let bytes = utxo.marshal()?;
        state.add_utxo(utxo_id, bytes);
    }
    Ok(())
}

/// Extract the owner addresses from an [`Output`] as their raw byte representations,
/// for use as `traits` in the atomic [`Element`].
///
/// In Go: `utxo.Out.Addresses()` — for `secp256k1fx.TransferOutput` / `MintOutput`
/// these are the `OutputOwners.Addrs` (each a 20-byte `ids.ShortID`).
//
// TODO(M5.5): add nftfx/propertyfx `Output` arms when they land. nftfx
// `TransferOutput`/`MintOutput` and propertyfx outputs carry their own owners;
// nftfx outputs with no owners contribute no traits (return empty).
#[must_use]
fn output_addresses(out: &Output) -> Vec<Vec<u8>> {
    let addrs = match out {
        Output::SecpTransfer(o) => &o.owners.addrs,
        Output::SecpMint(o) => &o.owners.addrs,
    };
    addrs.iter().map(|addr| addr.as_bytes().to_vec()).collect()
}
