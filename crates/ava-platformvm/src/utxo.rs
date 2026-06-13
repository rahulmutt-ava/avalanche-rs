// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-Chain UTXO handler — typed `avax.UTXO` (de)serialization +
//! spend/produce/verify over a [`state::Chain`](crate::state::chain::Chain)
//! overlay (`vms/platformvm/utxo`, `vms/components/avax`; specs 08 §2.4 / §1).
//!
//! ## ATOMIC-1 (specs 00 §11.1.7)
//!
//! A UTXO is exported by one chain (P/X/C) and consumed on another through
//! shared memory, so its wire bytes must decode identically everywhere. The
//! P-Chain codec registers `secp256k1fx` at the AVM-aligned type IDs (5/7/9/10/11
//! — see [`crate::txs::codec`]); marshalling a [`Utxo`] through the shared
//! [`crate::txs::codec::Codec`] manager therefore produces the canonical
//! `avax.UTXO` layout: codec version `0x0000` + `UTXOID{txID, outputIndex}` +
//! `AssetID` + the typed fx output (its own type_id prefix + payload).
//!
//! ## Handler model (M4.13 `UtxoBytes`)
//!
//! M4.13 stores UTXOs in the [`Diff`](crate::state::diff::Diff) /
//! [`State`](crate::state::state::State) as opaque [`UtxoBytes`] (their canonical
//! codec bytes). This handler owns the typed boundary: [`produce`] serializes the
//! created UTXOs and [`consume`] deletes the referenced ones, while
//! [`verify_spend`] enforces the balance equation `sum(consumed) ==
//! sum(produced) + fee` for a single asset (full multi-asset / locktime selection
//! and credential checks land in M4.16's executor).

use ava_codec::AvaCodec;
use ava_types::id::Id;

use crate::error::{Error, Result};
use crate::state::chain::Chain;
use crate::txs::components::{Output, TransferableInput, TransferableOutput};

/// `avax.UTXO` — a UTXOID + asset + fx output, the cross-chain / shared-memory
/// value layout (`vms/components/avax/utxo.go`, specs 08 §3.2).
///
/// Embeds the `UTXOID` (`tx_id`, `output_index`) and `Asset` (`asset_id`) inline,
/// exactly as Go's flattened `serialize` fields, then the fx output interface
/// (which carries its own registered type_id). Marshalling through the shared
/// P-Chain codec yields bytes identical to the X-Chain / C-Chain encoding
/// (ATOMIC-1).
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

    /// Marshals this UTXO to its canonical codec bytes (version prefix +
    /// `UTXOID` + `Asset` + typed fx output), the value persisted as
    /// [`UtxoBytes`](crate::state::chain::UtxoBytes) and exchanged across chains.
    ///
    /// # Errors
    /// Returns [`Error::Codec`] if encoding fails.
    pub fn marshal(&self) -> Result<Vec<u8>> {
        crate::txs::codec::Codec()
            .marshal(crate::CODEC_VERSION, self)
            .map_err(Error::Codec)
    }

    /// Unmarshals a UTXO from its canonical codec bytes.
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

/// `avax.Addressable.Addresses()` — the secp256k1 addresses owning an fx
/// output (`vms/components/avax`): a `secp256k1fx.TransferOutput` reports its
/// `OutputOwners.Addrs`; a `stakeable.LockOut` delegates to its wrapped
/// output. Used to maintain the address → UTXO index (`avax.utxoState`,
/// specs 08 §3.2).
#[must_use]
pub fn output_addresses(out: &Output) -> &[ava_types::short_id::ShortId] {
    match out {
        Output::Transfer(o) => &o.owners.addrs,
        Output::StakeableLock(l) => output_addresses(&l.transferable_out),
    }
}

/// `avax.Consume` — remove the UTXOs referenced by `ins` from the UTXO set
/// (`vms/components/avax/utxo_handler.go`).
pub fn consume(chain: &mut dyn Chain, ins: &[TransferableInput]) {
    for input in ins {
        chain.delete_utxo(input.input_id());
    }
}

/// `avax.Produce` — add the UTXOs created by `outs` to the UTXO set, keyed by
/// `(tx_id, index)` (`vms/components/avax/utxo_handler.go`).
///
/// # Errors
/// Returns [`Error::Codec`] if a produced UTXO cannot be marshaled, or
/// [`Error::Overflow`] if `outs` is longer than `u32::MAX`.
pub fn produce(chain: &mut dyn Chain, tx_id: Id, outs: &[TransferableOutput]) -> Result<()> {
    for (index, out) in outs.iter().enumerate() {
        let output_index = u32::try_from(index).map_err(|_| Error::Overflow)?;
        let utxo = Utxo {
            tx_id,
            output_index,
            asset_id: out.asset_id,
            out: out.out.clone(),
        };
        chain.add_utxo(utxo.input_id(), utxo.marshal()?);
    }
    Ok(())
}

/// Verifies the value-conservation invariant for a single-asset spend over
/// `chain`: `sum(consumed_in) == sum(produced_out) + fee`, where every input's
/// referenced UTXO must exist and match `asset_id` (specs 08 §2.4
/// `VerifySpendUTXOs`, single-asset / no-locktime slice; locktime + credential
/// checks land with the M4.16 executor).
///
/// # Errors
/// - [`Error::Database`] if a referenced UTXO is absent.
/// - [`Error::Codec`] if a referenced UTXO cannot be decoded.
/// - [`Error::InvalidComponent`] if an input's claimed asset id does not match
///   its consumed UTXO's asset id.
/// - [`Error::Overflow`] if any sum overflows `u64`.
/// - [`Error::InvalidComponent`] if `produced + fee != consumed`.
pub fn verify_spend(
    chain: &dyn Chain,
    ins: &[TransferableInput],
    outs: &[TransferableOutput],
    fee: u64,
    asset_id: Id,
) -> Result<()> {
    let mut consumed: u64 = 0;
    for input in ins {
        if input.asset_id != asset_id {
            return Err(Error::InvalidComponent);
        }
        let bytes = chain.get_utxo(input.input_id())?;
        let utxo = Utxo::unmarshal(&bytes)?;
        if utxo.asset_id != asset_id {
            return Err(Error::InvalidComponent);
        }
        consumed = consumed
            .checked_add(input.amount())
            .ok_or(Error::Overflow)?;
    }

    let mut produced: u64 = fee;
    for out in outs {
        if out.asset_id != asset_id {
            return Err(Error::InvalidComponent);
        }
        produced = produced.checked_add(out.amount()).ok_or(Error::Overflow)?;
    }

    if produced != consumed {
        return Err(Error::InvalidComponent);
    }
    Ok(())
}
