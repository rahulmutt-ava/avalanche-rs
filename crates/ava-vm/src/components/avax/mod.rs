// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `vms/components/avax` — the shared UTXO model (specs 07 §3.1).
//!
//! Byte-exact codec types: `serialize:"true"` fields are encoded in registration
//! order; the `fx_id` fields are `serialize:"false"` — runtime-only, set from the
//! registered fx, **never** encoded. The fx output/input payloads are
//! object-safe trait objects ([`TransferableOut`]/[`TransferableIn`]) so a VM can
//! mix fx output types in one tx.
//!
//! Sorting ([`sort_transferable_outputs`] / [`sort_transferable_inputs`] and the
//! `*_with_signers` variant) is **consensus-affecting** — the comparators
//! reproduce Go's exactly: outputs by `(asset_id, codec(out) bytes)`, inputs by
//! UTXOID (`tx_id` then `output_index`). [`FlowChecker`] is the per-asset
//! produce/consume ledger backing [`verify_tx`], using checked arithmetic
//! (`safemath`) and a deterministic [`BTreeMap`].

pub mod shared_memory;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use ava_snow::ChainContext;
use ava_types::id::Id;
use ava_utils::math as safemath;

use crate::components::verify::{self, State, Verifiable};
use crate::error::{Error, Result};

/// The maximum number of bytes in a tx memo field (`avax.MaxMemoSize`).
pub const MAX_MEMO_SIZE: usize = 256;

/// `avax.UTXOID` — references the UTXO an input spends.
///
/// `id` is derived lazily as `tx_id.prefix(output_index as u64)` (=
/// `sha256(be64(index) ++ tx_id)`) and cached in a [`OnceLock`]. `symbol`
/// (`Symbol` in Go) is runtime-only and marks a symbolic (non-DB) input.
#[derive(Debug, Default)]
pub struct UtxoId {
    /// The id of the tx that produced the referenced UTXO (`serialize`).
    pub tx_id: Id,
    /// The index of the referenced output within that tx (`serialize`).
    pub output_index: u32,
    /// `Symbol` — runtime-only; `true` for a symbolic (non-DB) input.
    pub symbol: bool,
    /// Lazily-derived, cached input id.
    id: OnceLock<Id>,
}

impl Clone for UtxoId {
    fn clone(&self) -> Self {
        // The cached id is reproducible, so copying it (when present) is sound;
        // start fresh when unset to keep `Clone` cheap and side-effect-free.
        let id = OnceLock::new();
        if let Some(v) = self.id.get() {
            let _ = id.set(*v);
        }
        Self {
            tx_id: self.tx_id,
            output_index: self.output_index,
            symbol: self.symbol,
            id,
        }
    }
}

impl PartialEq for UtxoId {
    fn eq(&self, other: &Self) -> bool {
        self.tx_id == other.tx_id
            && self.output_index == other.output_index
            && self.symbol == other.symbol
    }
}

impl Eq for UtxoId {}

impl UtxoId {
    /// Builds a [`UtxoId`] from a tx id and output index.
    #[must_use]
    pub fn new(tx_id: Id, output_index: u32) -> Self {
        Self {
            tx_id,
            output_index,
            symbol: false,
            id: OnceLock::new(),
        }
    }

    /// `InputID()` — the unique id of the UTXO this input spends,
    /// `tx_id.prefix(output_index)`. Cached after the first call.
    #[must_use]
    pub fn input_id(&self) -> Id {
        *self
            .id
            .get_or_init(|| self.tx_id.prefix(&[u64::from(self.output_index)]))
    }

    /// `InputSource()` — the `(tx_id, output_index)` pair.
    #[must_use]
    pub fn input_source(&self) -> (Id, u32) {
        (self.tx_id, self.output_index)
    }

    /// `Compare(other)` — order by `tx_id` bytes, then by `output_index`
    /// (consensus-affecting; matches Go's `UTXOID.Compare`).
    #[must_use]
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.tx_id
            .to_bytes()
            .cmp(&other.tx_id.to_bytes())
            .then_with(|| self.output_index.cmp(&other.output_index))
    }
}

impl Verifiable for UtxoId {
    fn verify(&self) -> Result<()> {
        Ok(())
    }
}

/// `avax.Asset` — the asset an output/input/UTXO is denominated in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Asset {
    /// The asset id (`serialize`).
    pub id: Id,
}

impl Asset {
    /// Builds an [`Asset`] from its id.
    #[must_use]
    pub fn new(id: Id) -> Self {
        Self { id }
    }

    /// `AssetID()` — the contained asset id.
    #[must_use]
    pub fn asset_id(&self) -> Id {
        self.id
    }
}

impl Verifiable for Asset {
    fn verify(&self) -> Result<()> {
        if self.id == Id::EMPTY {
            return Err(Error::InvalidComponent("empty asset ID is not valid"));
        }
        Ok(())
    }
}

/// `avax.TransferableOut` — an fx output that transfers value.
pub trait TransferableOut: State {
    /// `Amount()` — the value this output represents.
    fn amount(&self) -> u64;

    /// The canonical codec bytes of this output, used as the secondary sort key
    /// (Go marshals `Out` with the codec). Must be byte-identical to the encoded
    /// form on every node.
    fn codec_bytes(&self) -> Vec<u8>;
}

/// `avax.TransferableIn` — an fx input that transfers value.
pub trait TransferableIn: Verifiable + Send + Sync {
    /// `Amount()` — the value this input represents.
    fn amount(&self) -> u64;

    /// `Cost()` — how much this input costs to include.
    ///
    /// # Errors
    /// Returns a [`crate::error::Error`] if the cost cannot be computed.
    fn cost(&self) -> Result<u64>;
}

/// `avax.TransferableOutput` — an asset + fx output, sorted by
/// `(asset_id, codec(out) bytes)`.
#[derive(Clone)]
pub struct TransferableOutput {
    /// The asset being transferred (`serialize`, embedded).
    pub asset: Asset,
    /// `FxID` — runtime-only (`serialize:"false"`); never encoded.
    pub fx_id: Id,
    /// The fx output payload (`serialize`).
    pub out: Arc<dyn TransferableOut>,
}

impl TransferableOutput {
    /// `AssetID()` — the contained asset id.
    #[must_use]
    pub fn asset_id(&self) -> Id {
        self.asset.id
    }

    /// `Output()` — the fx output payload.
    #[must_use]
    pub fn output(&self) -> &Arc<dyn TransferableOut> {
        &self.out
    }
}

impl Verifiable for TransferableOutput {
    fn verify(&self) -> Result<()> {
        self.asset.verify()?;
        self.out.verify()
    }
}

/// `avax.TransferableInput` — a UTXOID + asset + fx input, sorted+unique by
/// UTXOID.
#[derive(Clone)]
pub struct TransferableInput {
    /// The UTXO this input spends (`serialize`, embedded).
    pub utxo_id: UtxoId,
    /// The asset being spent (`serialize`, embedded).
    pub asset: Asset,
    /// `FxID` — runtime-only (`serialize:"false"`); never encoded.
    pub fx_id: Id,
    /// The fx input payload (`serialize`).
    pub r#in: Arc<dyn TransferableIn>,
}

impl TransferableInput {
    /// `AssetID()` — the contained asset id.
    #[must_use]
    pub fn asset_id(&self) -> Id {
        self.asset.id
    }

    /// `Input()` — the fx input payload.
    #[must_use]
    pub fn input(&self) -> &Arc<dyn TransferableIn> {
        &self.r#in
    }

    /// `Compare(other)` — by the input's UTXOID (consensus-affecting).
    #[must_use]
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.utxo_id.compare(&other.utxo_id)
    }
}

impl Verifiable for TransferableInput {
    fn verify(&self) -> Result<()> {
        self.utxo_id.verify()?;
        self.asset.verify()?;
        self.r#in.verify()
    }
}

/// `avax.UTXO` — an on-chain unspent output.
#[derive(Clone)]
pub struct Utxo {
    /// Identifies this UTXO (`serialize`, embedded).
    pub utxo_id: UtxoId,
    /// The asset this UTXO holds (`serialize`, embedded).
    pub asset: Asset,
    /// The fx output state (`serialize`).
    pub out: Arc<dyn State>,
}

impl Verifiable for Utxo {
    fn verify(&self) -> Result<()> {
        self.utxo_id.verify()?;
        self.asset.verify()?;
        self.out.verify()
    }
}

/// `SortTransferableOutputs` — sort by `(asset_id, codec(out) bytes)`.
///
/// Consensus-affecting: matches Go's `innerSortTransferableOutputs.Less`.
pub fn sort_transferable_outputs(outs: &mut [TransferableOutput]) {
    outs.sort_by(|a, b| {
        a.asset
            .id
            .to_bytes()
            .cmp(&b.asset.id.to_bytes())
            .then_with(|| a.out.codec_bytes().cmp(&b.out.codec_bytes()))
    });
}

/// `IsSortedTransferableOutputs` — true iff `outs` are in canonical order.
#[must_use]
pub fn is_sorted_transferable_outputs(outs: &[TransferableOutput]) -> bool {
    outs.windows(2).all(|w| match w {
        [a, b] => {
            let ord = a
                .asset
                .id
                .to_bytes()
                .cmp(&b.asset.id.to_bytes())
                .then_with(|| a.out.codec_bytes().cmp(&b.out.codec_bytes()));
            ord != std::cmp::Ordering::Greater
        }
        _ => true,
    })
}

/// `SortTransferableInputs` — sort by UTXOID.
pub fn sort_transferable_inputs(ins: &mut [TransferableInput]) {
    ins.sort_by(|a, b| a.compare(b));
}

/// `SortTransferableInputsWithSigners` — sort the inputs by UTXOID and permute
/// the parallel `signers` slice in lockstep (consensus-affecting; the signer
/// rows must track their inputs).
///
/// `signers[i]` is the signer set for `ins[i]`. Panics-free: if the slices
/// differ in length, only the common prefix is reordered together (mirroring
/// Go, which always passes equal-length slices).
pub fn sort_transferable_inputs_with_signers<S>(ins: &mut Vec<TransferableInput>, signers: &mut [S])
where
    S: Clone,
{
    // Sort an index permutation by the input comparator, then apply it to both.
    let mut order: Vec<usize> = (0..ins.len()).collect();
    order.sort_by(|&i, &j| match (ins.get(i), ins.get(j)) {
        (Some(a), Some(b)) => a.compare(b),
        _ => std::cmp::Ordering::Equal,
    });

    let new_ins: Vec<TransferableInput> =
        order.iter().filter_map(|&i| ins.get(i).cloned()).collect();
    *ins = new_ins;

    if signers.len() == order.len() {
        let new_signers: Vec<S> = order
            .iter()
            .filter_map(|&i| signers.get(i).cloned())
            .collect();
        signers.clone_from_slice(&new_signers);
    }
}

/// `IsSortedAndUniqueTransferableInputs` — true iff `ins` are strictly
/// increasing by UTXOID (sorted and unique).
#[must_use]
pub fn is_sorted_and_unique_transferable_inputs(ins: &[TransferableInput]) -> bool {
    ins.windows(2).all(|w| match w {
        [a, b] => a.compare(b) == std::cmp::Ordering::Less,
        _ => true,
    })
}

/// `avax.FlowChecker` — the produce/consume per-asset balance ledger.
///
/// Uses a [`BTreeMap`] (never a `HashMap` on a consensus path — specs 00 §6.1)
/// and checked arithmetic (`safemath`). A balance check passes iff `consumed >=
/// produced` for every produced asset.
#[derive(Debug, Default)]
pub struct FlowChecker {
    consumed: BTreeMap<Id, u64>,
    produced: BTreeMap<Id, u64>,
    /// The first arithmetic error encountered, if any (mirrors Go's `errs`).
    err: Option<Error>,
}

impl FlowChecker {
    /// Builds an empty [`FlowChecker`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn add(map: &mut BTreeMap<Id, u64>, asset_id: Id, amount: u64) -> Result<()> {
        let entry = map.entry(asset_id).or_insert(0);
        *entry = safemath::add(*entry, amount).map_err(|_| Error::Overflow)?;
        Ok(())
    }

    /// `Consume(assetID, amount)` — record consumed value for an asset.
    pub fn consume(&mut self, asset_id: Id, amount: u64) {
        if self.err.is_some() {
            return;
        }
        if let Err(e) = Self::add(&mut self.consumed, asset_id, amount) {
            self.err = Some(e);
        }
    }

    /// `Produce(assetID, amount)` — record produced (incl. burned-fee) value.
    pub fn produce(&mut self, asset_id: Id, amount: u64) {
        if self.err.is_some() {
            return;
        }
        if let Err(e) = Self::add(&mut self.produced, asset_id, amount) {
            self.err = Some(e);
        }
    }

    /// `Verify()` — succeeds iff no arithmetic error occurred and every produced
    /// asset is fully covered by what was consumed.
    ///
    /// # Errors
    /// Returns the first arithmetic error, or [`Error::InsufficientFunds`] if
    /// `produced > consumed` for some asset.
    pub fn verify(&self) -> Result<()> {
        if let Some(e) = &self.err {
            return Err(clone_arith_error(e));
        }
        for (asset_id, &produced) in &self.produced {
            let consumed = self.consumed.get(asset_id).copied().unwrap_or(0);
            if produced > consumed {
                return Err(Error::InsufficientFunds);
            }
        }
        Ok(())
    }
}

/// Clones the (always arithmetic) error a [`FlowChecker`] retains; the variants
/// it can hold (`Overflow`/`Underflow`) carry no non-`Copy` payload.
fn clone_arith_error(e: &Error) -> Error {
    match e {
        Error::Underflow => Error::Underflow,
        // Any retained error is an arithmetic add overflow.
        _ => Error::Overflow,
    }
}

/// `avax.VerifyTx` — verify inputs/outputs flow-check (incl. a burned fee) and
/// are sorted.
///
/// `all_ins`/`all_outs` are grouped (one sub-slice per "credential set", as in
/// the P/X-Chain txs); each sub-slice must be independently sorted.
///
/// # Errors
/// Returns the first verification error: a component `verify` failure,
/// [`Error::OutputsNotSorted`] / [`Error::InputsNotSortedUnique`], or the
/// [`FlowChecker`] verdict.
pub fn verify_tx(
    fee_amount: u64,
    fee_asset_id: Id,
    all_ins: &[Vec<TransferableInput>],
    all_outs: &[Vec<TransferableOutput>],
) -> Result<()> {
    let mut fc = FlowChecker::new();

    // The fee must be burned (produced but not consumed).
    fc.produce(fee_asset_id, fee_amount);

    for outs in all_outs {
        for out in outs {
            out.verify()?;
            fc.produce(out.asset_id(), out.output().amount());
        }
        if !is_sorted_transferable_outputs(outs) {
            return Err(Error::OutputsNotSorted);
        }
    }

    for ins in all_ins {
        for input in ins {
            input.verify()?;
            fc.consume(input.asset_id(), input.input().amount());
        }
        if !is_sorted_and_unique_transferable_inputs(ins) {
            return Err(Error::InputsNotSortedUnique);
        }
    }

    fc.verify()
}

/// `avax.Metadata` — the cached unsigned/signed bytes + id of a tx.
///
/// `OnceLock<(bytes, id)>` is populated at `initialize` (Go's
/// `Metadata.Initialize`); reads after that are cheap and consistent.
#[derive(Debug, Default)]
pub struct Metadata {
    cached: OnceLock<(Vec<u8>, Id)>,
}

impl Metadata {
    /// Builds an uninitialized [`Metadata`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `Initialize(unsignedBytes, signedBytes)` — caches the canonical bytes +
    /// derived id. Subsequent calls are ignored (the first wins).
    pub fn initialize(&self, bytes: Vec<u8>, id: Id) {
        let _ = self.cached.set((bytes, id));
    }

    /// `Bytes()` — the cached canonical bytes, if initialized.
    #[must_use]
    pub fn bytes(&self) -> Option<&[u8]> {
        self.cached.get().map(|(b, _)| b.as_slice())
    }

    /// `ID()` — the cached id, if initialized.
    #[must_use]
    pub fn id(&self) -> Option<Id> {
        self.cached.get().map(|(_, id)| *id)
    }
}

/// `avax.BaseTx` — the common preamble embedded by P-Chain/X-Chain txs.
pub struct BaseTx {
    /// The network this chain lives on (`serialize`).
    pub network_id: u32,
    /// The chain this tx exists on — prevents cross-chain replay (`serialize`).
    pub blockchain_id: Id,
    /// The tx outputs (`serialize`).
    pub outs: Vec<TransferableOutput>,
    /// The tx inputs (`serialize`).
    pub ins: Vec<TransferableInput>,
    /// Arbitrary memo bytes, up to [`MAX_MEMO_SIZE`] (`serialize`).
    pub memo: Vec<u8>,
}

impl BaseTx {
    /// `InputUTXOs()` — the UTXOIDs this tx consumes.
    #[must_use]
    pub fn input_utxos(&self) -> Vec<&UtxoId> {
        self.ins.iter().map(|i| &i.utxo_id).collect()
    }

    /// `NumCredentials()` — the number of expected credentials (one per input).
    #[must_use]
    pub fn num_credentials(&self) -> usize {
        self.ins.len()
    }

    /// `Verify(ctx)` — validate the tx preamble against the chain context.
    ///
    /// # Errors
    /// Returns [`Error::InvalidComponent`] on a wrong network/chain id or an
    /// oversized memo.
    pub fn verify(&self, ctx: &ChainContext) -> Result<()> {
        if self.network_id != ctx.network_id {
            return Err(Error::InvalidComponent("tx has wrong network ID"));
        }
        if self.blockchain_id != ctx.chain_id {
            return Err(Error::InvalidComponent("tx has wrong chain ID"));
        }
        if self.memo.len() > MAX_MEMO_SIZE {
            return Err(Error::InvalidComponent("memo exceeds maximum length"));
        }
        Ok(())
    }
}

/// `verify.All` over avax components — re-exported for convenience.
///
/// # Errors
/// Returns the first item's verification error.
pub fn verify_all(items: &[&dyn Verifiable]) -> Result<()> {
    verify::all(items)
}
