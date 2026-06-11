// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-chain wallet backend — a pure snapshot of the UTXO set and the
//! owner registry (port of `wallet/chain/p/wallet/backend.go`, reduced to the
//! state the builder/signer read; specs 12 §13).

use std::collections::BTreeMap;

use ava_platformvm::utxo::Utxo;
use ava_secp256k1fx::OutputOwners;
use ava_types::id::Id;

/// The state snapshot the builder (`UTXOs`/`GetOwner`) and signer
/// (`GetUTXO`/`GetOwner`) read. Implementations MUST be pure (no I/O) and
/// return UTXOs in the canonical deterministic order
/// ([`crate::common::utxo_select::sort_utxos`]).
pub trait Backend {
    /// `Backend.UTXOs` — every UTXO spendable on the P-chain that was sourced
    /// from `source_chain_id` (the P-chain id itself for local UTXOs, another
    /// chain id for atomic imports), in canonical order.
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo>;

    /// `Backend.GetUTXO` — a single UTXO by its `InputID`.
    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo>;

    /// `Backend.GetOwner` — the owner of a subnet / validation id.
    fn get_owner(&self, owner_id: Id) -> Option<OutputOwners>;
}

/// An in-memory snapshot backend (the deterministic equivalent of Go's
/// `wallet.NewBackend(chainUTXOs, owners)` over `utxotest`'s sorted UTXOs).
#[derive(Default)]
pub struct WalletBackend {
    utxos: BTreeMap<Id, Vec<Utxo>>,
    owners: BTreeMap<Id, OutputOwners>,
}

impl WalletBackend {
    /// Builds a snapshot from per-source-chain UTXO sets + the owner registry.
    /// The UTXO lists are sorted into canonical order.
    #[must_use]
    pub fn new(utxo_sets: BTreeMap<Id, Vec<Utxo>>, owners: BTreeMap<Id, OutputOwners>) -> Self {
        let mut utxos = utxo_sets;
        for set in utxos.values_mut() {
            crate::common::utxo_select::sort_utxos(set);
        }
        Self { utxos, owners }
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

    fn get_owner(&self, owner_id: Id) -> Option<OutputOwners> {
        self.owners.get(&owner_id).cloned()
    }
}
