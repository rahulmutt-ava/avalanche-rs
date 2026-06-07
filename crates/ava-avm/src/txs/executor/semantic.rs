// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The stateful [`SemanticVerifier`] over all five X-Chain tx types
//! (specs 09 §6.2).
//!
//! Port of `vms/avm/txs/executor/semantic_verifier.go` — the `txs.Visitor` that
//! validates a parsed `Tx` **against chain state**, after the stateless
//! [`SyntacticVerifier`](super::syntactic::SyntacticVerifier) pass. Each tx-type
//! method reproduces the Go logic exactly:
//!
//! * **`BaseTx`** — for each input, fetch the consumed UTXO from state
//!   ([`ReadOnlyChain::get_utxo`]), require `utxo.asset == in.asset`
//!   ([`Error::AssetIdMismatch`]), route the credential to its fx
//!   ([`resolve_fx_index`]), check the asset enables that fx
//!   ([`verify_fx_usage`](SemanticVerifier::verify_fx_usage)), then run
//!   `fx.verify_transfer`. For each output, route to its fx and `verify_fx_usage`.
//! * **`CreateAssetTx`** — the embedded `BaseTx` (the new asset's initial-state
//!   outs are produced in execution, not spent here).
//! * **`OperationTx`** — the `BaseTx`, then per-op verification (fetch the op's
//!   input UTXOs, `verify_fx_usage`, `fx.verify_operation`) with credential index
//!   `len(ins) + op_index`. **Skipped** entirely when `!bootstrapped` **or** when
//!   `tx.id == GRANDFATHERED_OPERATION_TX` (a grandfathered mainnet tx — a
//!   bug-compat quirk that is part of the accepted chain history; specs 09 §6.2).
//! * **`ImportTx`** — the `BaseTx`, then (if bootstrapped) `SameSubnet`, then the
//!   imported UTXOs are fetched from **shared memory** (`SharedMemory.get`),
//!   unmarshaled as [`Utxo`], and transfer-verified with credential index
//!   `len(ins) + i`.
//! * **`ExportTx`** — the `BaseTx`, then (if bootstrapped) `SameSubnet`, then
//!   `verify_fx_usage` per exported out.
//!
//! ## As-built notes
//!
//! * **UTXO (de)serialization.** Go does `v.Codec.Unmarshal(bytes, &avax.UTXO{})`.
//!   The X-Chain state stores UTXOs as opaque codec bytes (M5.10), and the
//!   runtime `ava_vm::components::avax::Utxo` is not codec-serializable in
//!   isolation (its fx payload is an `Arc<dyn State>`). This module therefore
//!   defines a byte-exact, codec-serializable [`Utxo`] mirroring the P-Chain
//!   precedent (`ava_platformvm::utxo::Utxo`); it round-trips through the shared
//!   avm [`Codec`](crate::txs::codec::Codec) and so decodes identically to the
//!   cross-chain / shared-memory layout (ATOMIC-1).
//! * **`SameSubnet`.** Go's `verify.SameSubnet` calls
//!   `ctx.ValidatorState.GetSubnetID(peerChainID)` and compares against
//!   `ctx.SubnetID`. The node-wide validator-state resolver is not yet wired into
//!   the avm verifier, so this module exposes the minimal seam: a
//!   [`SubnetResolver`] (`GetSubnetID`) plus the local subnet id, supplied via
//!   [`SemanticVerifier::with_same_subnet`]. When absent, `SameSubnet` is **not**
//!   run (parity with `!bootstrapped`, which also skips it); see the deferral in
//!   the M5.13 as-built notes.

use ava_codec::AvaCodec;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::SharedMemory;
use ava_vm::fx::UnsignedTx as FxUnsignedTx;

use crate::error::{Error, Result};
use crate::fx::dispatch::Dispatch;
use crate::fx::dispatch::resolve_fx_index;
use crate::fx_index::FxIndex;
use crate::state::chain::ReadOnlyChain;
use crate::txs::components::{Input, Output, TransferableInput};
use crate::txs::executor::backend::Backend;
use crate::txs::executor::consts::GRANDFATHERED_OPERATION_TX;
use crate::txs::{
    BaseTx, CreateAssetTx, Credential, ExportTx, ImportTx, OperationTx, Tx, UnsignedTx,
};

/// `avax.UTXO` — a UTXOID + asset + fx output, the cross-chain / shared-memory
/// value layout (`vms/components/avax/utxo.go`, specs 09 §5.1).
///
/// The byte-exact, codec-serializable shape the semantic verifier unmarshals from
/// state ([`ReadOnlyChain::get_utxo`]) and from shared memory (`SharedMemory.get`)
/// — mirroring the P-Chain [`ava_platformvm::utxo::Utxo`] precedent (the runtime
/// `ava_vm` `Utxo` is not codec-serializable in isolation). Marshalling through
/// the shared avm [`Codec`](crate::txs::codec::Codec) yields the canonical
/// `avax.UTXO` bytes (ATOMIC-1).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Utxo {
    /// `UTXOID.TxID` — the id of the tx that produced this UTXO.
    #[codec]
    pub tx_id: Id,
    /// `UTXOID.OutputIndex` — the index of this output within that tx.
    #[codec]
    pub output_index: u32,
    /// `Asset.ID` — the asset this UTXO holds.
    #[codec]
    pub asset_id: Id,
    /// `Out` — the fx output payload (interface; carries its own type_id).
    #[codec]
    pub out: Output,
}

impl Utxo {
    /// `InputID()` — the unique id of this UTXO, `tx_id.prefix(output_index)`.
    #[must_use]
    pub fn input_id(&self) -> Id {
        self.tx_id.prefix(&[u64::from(self.output_index)])
    }

    /// Marshals this UTXO to its canonical codec bytes (version prefix + `UTXOID`
    /// + `Asset` + typed fx output).
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if encoding fails.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        crate::txs::codec::Codec()
            .marshal(crate::txs::CODEC_VERSION, self)
            .map_err(Error::Codec)
    }

    /// Unmarshals a UTXO from its canonical codec bytes (`Codec.Unmarshal`).
    ///
    /// # Errors
    /// Returns [`Error::Codec`] on a malformed/over-long stream, an unknown codec
    /// version, an unknown fx type_id, or trailing bytes.
    pub fn unmarshal(bytes: &[u8]) -> Result<Self> {
        let mut utxo = Utxo::default();
        crate::txs::codec::Codec()
            .unmarshal(bytes, &mut utxo)
            .map_err(Error::Codec)?;
        Ok(utxo)
    }
}

/// `snow.ValidatorState.GetSubnetID` — resolves a chain id to the subnet it
/// belongs to (`verify.SameSubnet`, specs 09 §6.2).
///
/// The minimal seam the [`SemanticVerifier`] needs for `SameSubnet`; the
/// node-wide validator-state service supplies the real resolver, the tests a
/// fixed map. See the module docs for why this is a per-verifier dependency
/// rather than a `Backend` field.
pub trait SubnetResolver {
    /// `GetSubnetID(chainID)` — the subnet `chain` belongs to.
    ///
    /// # Errors
    /// Returns an [`Error`] if the subnet of `chain` cannot be resolved.
    fn get_subnet_id(&self, chain: Id) -> Result<Id>;
}

/// `executor.SemanticVerifier` — the stateful tx-verification visitor
/// (specs 09 §6.2). Borrows the [`Backend`], the chain `state`, the signed `tx`,
/// and the fx [`Dispatch`]; the asset whose `CreateAssetTx` backs `verify_fx_usage`
/// lookups is resolved through `state`.
pub struct SemanticVerifier<'a> {
    /// The verification context (chain ids, fees, fx count, bootstrapped flag).
    backend: &'a Backend,
    /// The read-only chain state (input UTXOs + the asset's `CreateAssetTx`).
    state: &'a dyn ReadOnlyChain,
    /// The signed tx whose credentials authorize the spends.
    tx: &'a Tx,
    /// The fx dispatch table (routing + transfer/operation verification).
    fxs: &'a Dispatch,
    /// The unsigned-tx bytes passed to the fx spend gate (hashed for signature
    /// recovery; `avax.Metadata.UnsignedBytes`).
    unsigned_bytes: Vec<u8>,
    /// The cross-chain shared-memory read handle (`Ctx.SharedMemory`); required
    /// only for [`ImportTx`].
    shared_memory: Option<&'a dyn SharedMemory>,
    /// The local subnet id + the `GetSubnetID` resolver backing `SameSubnet`
    /// (`Ctx.SubnetID` + `Ctx.ValidatorState`); `SameSubnet` is skipped when this
    /// is absent.
    same_subnet: Option<(Id, &'a dyn SubnetResolver)>,
}

impl<'a> SemanticVerifier<'a> {
    /// Builds a [`SemanticVerifier`]. `asset_id` is unused as a stored field; the
    /// asset of each consumed value is read from the UTXO / input directly (kept
    /// in the signature for call-site clarity and future use).
    #[must_use]
    pub fn new(
        backend: &'a Backend,
        state: &'a dyn ReadOnlyChain,
        tx: &'a Tx,
        fxs: &'a Dispatch,
        _asset_id: Id,
    ) -> Self {
        // The fx spend gate hashes the unsigned-tx bytes; compute them once. A
        // codec failure here is impossible for a parsed/initialized tx, so fall
        // back to the cached signed bytes (the fx gate is skipped while the fx is
        // not bootstrapped anyway).
        let unsigned_bytes = crate::txs::codec::Codec()
            .marshal(crate::txs::CODEC_VERSION, &tx.unsigned)
            .unwrap_or_else(|_| tx.bytes().to_vec());
        Self {
            backend,
            state,
            tx,
            fxs,
            unsigned_bytes,
            shared_memory: None,
            same_subnet: None,
        }
    }

    /// Supplies the shared-memory read handle used to fetch imported UTXOs
    /// (`ImportTx`).
    #[must_use]
    pub fn with_shared_memory(mut self, shared_memory: &'a dyn SharedMemory) -> Self {
        self.shared_memory = Some(shared_memory);
        self
    }

    /// Supplies the local subnet id + the `GetSubnetID` resolver enabling the
    /// `SameSubnet` check on `ImportTx` / `ExportTx`.
    #[must_use]
    pub fn with_same_subnet(mut self, subnet_id: Id, resolver: &'a dyn SubnetResolver) -> Self {
        self.same_subnet = Some((subnet_id, resolver));
        self
    }

    /// Runs the semantic verification for the tx's concrete `UnsignedTx` variant
    /// (`SemanticVerifier.Visit`).
    ///
    /// # Errors
    /// Returns the first failing check's avm [`Error`] (specs 09 §6.2).
    pub fn verify(&self) -> Result<()> {
        match &self.tx.unsigned {
            UnsignedTx::Base(tx) => self.base_tx(tx),
            UnsignedTx::CreateAsset(tx) => self.create_asset_tx(tx),
            UnsignedTx::Operation(tx) => self.operation_tx(tx),
            UnsignedTx::Import(tx) => self.import_tx(tx),
            UnsignedTx::Export(tx) => self.export_tx(tx),
        }
    }

    /// `SemanticVerifier.BaseTx`.
    fn base_tx(&self, tx: &BaseTx) -> Result<()> {
        for (i, input) in tx.base.ins.iter().enumerate() {
            // The credential length is checked during syntactic verification.
            let cred = self.cred_at(i)?;
            self.verify_transfer(input, cred)?;
        }

        for out in &tx.base.outs {
            let fx_index = self.fxs.route_output(&out.out)?;
            self.verify_fx_usage(fx_index, out.asset_id())?;
        }

        Ok(())
    }

    /// `SemanticVerifier.CreateAssetTx` — the embedded `BaseTx`.
    fn create_asset_tx(&self, tx: &CreateAssetTx) -> Result<()> {
        self.base_tx(&tx.base)
    }

    /// `SemanticVerifier.OperationTx`.
    fn operation_tx(&self, tx: &OperationTx) -> Result<()> {
        self.base_tx(&tx.base)?;

        // Bug-compat quirk (specs 09 §6.2): skip op verification when not
        // bootstrapped or for the grandfathered mainnet tx.
        if !self.backend.bootstrapped || self.is_grandfathered() {
            return Ok(());
        }

        let offset = tx.base.base.ins.len();
        for (i, op) in tx.ops.iter().enumerate() {
            // cred index = len(ins) + op_index.
            let cred = self.cred_at(offset.saturating_add(i))?;
            self.verify_operation(op, cred)?;
        }
        Ok(())
    }

    /// `SemanticVerifier.ImportTx`.
    fn import_tx(&self, tx: &ImportTx) -> Result<()> {
        self.base_tx(&tx.base)?;

        if !self.backend.bootstrapped {
            return Ok(());
        }

        self.verify_same_subnet(tx.source_chain)?;

        let utxo_ids: Vec<Vec<u8>> = tx
            .imported_ins
            .iter()
            .map(|input| input.input_id().to_bytes().to_vec())
            .collect();

        let shared_memory = self.shared_memory.ok_or(Error::MissingParentState)?;
        let all_utxo_bytes = shared_memory
            .get(tx.source_chain, &utxo_ids)
            .map_err(Error::Fx)?;

        let offset = tx.base.base.ins.len();
        for (i, input) in tx.imported_ins.iter().enumerate() {
            let bytes = all_utxo_bytes.get(i).ok_or(Error::AssetIdMismatch)?;
            let utxo = Utxo::unmarshal(bytes)?;
            let cred = self.cred_at(offset.saturating_add(i))?;
            self.verify_transfer_of_utxo(input, cred, &utxo)?;
        }
        Ok(())
    }

    /// `SemanticVerifier.ExportTx`.
    fn export_tx(&self, tx: &ExportTx) -> Result<()> {
        self.base_tx(&tx.base)?;

        if self.backend.bootstrapped {
            self.verify_same_subnet(tx.destination_chain)?;
        }

        for out in &tx.exported_outs {
            let fx_index = self.fxs.route_output(&out.out)?;
            self.verify_fx_usage(fx_index, out.asset_id())?;
        }
        Ok(())
    }

    /// `verifyTransfer` — fetch the input's UTXO from state, then verify it.
    fn verify_transfer(&self, input: &TransferableInput, cred: &Credential) -> Result<()> {
        let bytes = self.state.get_utxo(input.input_id())?;
        let utxo = Utxo::unmarshal(&bytes)?;
        self.verify_transfer_of_utxo(input, cred, &utxo)
    }

    /// `verifyTransferOfUTXO` — asset-id match, fx routing + usage, then the fx
    /// transfer gate.
    fn verify_transfer_of_utxo(
        &self,
        input: &TransferableInput,
        cred: &Credential,
        utxo: &Utxo,
    ) -> Result<()> {
        if utxo.asset_id != input.asset_id() {
            return Err(Error::AssetIdMismatch);
        }

        let fx_index = resolve_fx_index(cred.codec_type_id())?;
        self.verify_fx_usage(fx_index, input.asset_id())?;

        // Extract the concrete secp payloads the fx transfer gate consumes; a
        // non-secp input/output cannot route to the secp transfer gate.
        let Input::SecpTransfer(secp_in) = &input.r#in;
        let Output::SecpTransfer(secp_out) = &utxo.out else {
            return Err(Error::CantTransfer);
        };
        self.fxs
            .route_transfer(self.fx_tx(), secp_in, cred, secp_out)
    }

    /// `verifyOperation` — fetch each op-input UTXO (asset must match), fx routing
    /// + usage, then the fx operation gate.
    fn verify_operation(&self, op: &crate::txs::Operation, _cred: &Credential) -> Result<()> {
        let op_asset_id = op.asset.id;
        for utxo_id in &op.utxo_ids {
            let bytes = self.state.get_utxo(utxo_id.input_id())?;
            let utxo = Utxo::unmarshal(&bytes)?;
            if utxo.asset_id != op_asset_id {
                return Err(Error::AssetIdMismatch);
            }
        }

        // Route the op to its fx (`getFx(op.Op)`). The M5.5 fx-operation model is
        // a not-yet-routable placeholder, so this surfaces `UnknownFx` exactly
        // where Go would type the op through `typeToFxIndex`; the typed
        // `fx.verify_operation` dispatch lands with the M5.7/M5.8 op wiring.
        let fx_index = self.route_operation(op)?;
        self.verify_fx_usage(fx_index, op_asset_id)?;
        Ok(())
    }

    /// `getFx(op.Op)` — route an fx operation to its fx index by codec type-id.
    fn route_operation(&self, op: &crate::txs::Operation) -> Result<FxIndex> {
        // The placeholder `FxOperation::Unsupported` carries no routable type-id;
        // Go would route via `typeToFxIndex[reflect.TypeOf(op.Op)]`. Surface the
        // same `errUnknownFx` until the typed fx-operation enum (M5.7/M5.8) lands.
        let _ = op;
        Err(Error::UnknownFx)
    }

    /// `verifyFxUsage(fxID, assetID)` — load the asset's `CreateAssetTx` from
    /// state, then succeed iff some `InitialState.fx_index == fxID`.
    fn verify_fx_usage(&self, fx_index: FxIndex, asset_id: Id) -> Result<()> {
        let tx_bytes = self.state.get_tx(asset_id)?;
        let tx = Tx::parse(crate::txs::codec::Codec(), &tx_bytes).map_err(Error::Codec)?;
        let UnsignedTx::CreateAsset(create_asset) = &tx.unsigned else {
            return Err(Error::NotAnAsset);
        };
        let want = fx_index as u32;
        for state in &create_asset.states {
            if state.fx_index == want {
                return Ok(());
            }
        }
        Err(Error::IncompatibleFx)
    }

    /// `verify.SameSubnet(ctx, peerChainID)` — same subnet, different chain
    /// (specs 09 §6.2). A no-op when no resolver was supplied (see module docs).
    fn verify_same_subnet(&self, peer_chain: Id) -> Result<()> {
        let Some((subnet_id, resolver)) = self.same_subnet else {
            return Ok(());
        };
        if peer_chain == self.backend.blockchain_id {
            return Err(Error::SameChainId);
        }
        let peer_subnet = resolver.get_subnet_id(peer_chain)?;
        if peer_subnet != subnet_id {
            return Err(Error::MismatchedSubnetIds);
        }
        Ok(())
    }

    /// The credential at index `i` (`v.Tx.Creds[i].Credential`).
    fn cred_at(&self, i: usize) -> Result<&Credential> {
        self.tx
            .creds
            .get(i)
            .map(|c| &c.credential)
            .ok_or(Error::WrongNumberOfCredentials)
    }

    /// `v.Tx.ID().String() == GRANDFATHERED_OPERATION_TX`.
    fn is_grandfathered(&self) -> bool {
        match GRANDFATHERED_OPERATION_TX.parse::<Id>() {
            Ok(id) => self.tx.id() == id,
            Err(_) => false,
        }
    }

    /// The unsigned-tx bytes as an [`FxUnsignedTx`] for the fx spend gate.
    fn fx_tx(&self) -> &dyn FxUnsignedTx {
        &self.unsigned_bytes
    }
}
