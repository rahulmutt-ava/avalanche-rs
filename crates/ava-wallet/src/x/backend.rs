// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-chain wallet backend — a pure snapshot of the UTXO set (port of
//! `wallet/chain/x/backend.go`, reduced to the state the builder/signer read).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use ava_avm::txs::executor::semantic::Utxo;
use ava_types::id::Id;

/// Canonical UTXO order — `UTXOID.Compare`: `(tx_id bytes, output_index)`.
#[must_use]
pub fn cmp_utxo_ids(a: &Utxo, b: &Utxo) -> Ordering {
    a.tx_id
        .to_bytes()
        .cmp(&b.tx_id.to_bytes())
        .then_with(|| a.output_index.cmp(&b.output_index))
}

/// Sorts X-chain UTXOs into the canonical (deterministic) selection order.
pub fn sort_utxos(utxos: &mut [Utxo]) {
    utxos.sort_by(cmp_utxo_ids);
}

/// The state snapshot the builder (`UTXOs`) and signer (`GetUTXO`) read.
/// Implementations MUST be pure (no I/O) and return UTXOs in the canonical
/// deterministic order.
pub trait Backend {
    /// `Backend.UTXOs` — every UTXO spendable on the X-chain sourced from
    /// `source_chain_id`, in canonical order.
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo>;

    /// `SignerBackend.GetUTXO` — a single UTXO by its `InputID`.
    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo>;
}

/// An in-memory snapshot backend.
#[derive(Default)]
pub struct WalletBackend {
    utxos: BTreeMap<Id, Vec<Utxo>>,
}

impl WalletBackend {
    /// Builds a snapshot from per-source-chain UTXO sets, sorted canonically.
    #[must_use]
    pub fn new(utxo_sets: BTreeMap<Id, Vec<Utxo>>) -> Self {
        let mut utxos = utxo_sets;
        for set in utxos.values_mut() {
            sort_utxos(set);
        }
        Self { utxos }
    }
}

impl Backend for WalletBackend {
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo> {
        self.utxos
            .get(&source_chain_id)
            .cloned()
            .unwrap_or_default()
    }

    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo> {
        self.utxos
            .get(&source_chain_id)?
            .iter()
            .find(|u| u.input_id() == utxo_id)
            .cloned()
    }
}
