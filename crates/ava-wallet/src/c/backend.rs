// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The C-chain wallet backend — a pure snapshot of the atomic UTXO set plus
//! the EVM account balances/nonces (port of `wallet/chain/c/backend.go`,
//! reduced to the state the builder/signer read).

use std::collections::BTreeMap;

use ava_avm::txs::executor::semantic::Utxo;
use ava_types::id::Id;

use crate::x::backend::sort_utxos;

/// The state snapshot the builder (`UTXOs`/`Balance`/`Nonce`) and signer
/// (`GetUTXO`) read. Implementations MUST be pure (no I/O) and return UTXOs
/// in the canonical deterministic order.
pub trait Backend {
    /// `BuilderBackend.UTXOs` — every atomic UTXO exported to the C-chain from
    /// `source_chain_id`, in canonical order.
    fn utxos(&self, source_chain_id: Id) -> Vec<Utxo>;

    /// `SignerBackend.GetUTXO` — a single UTXO by its `InputID`.
    fn get_utxo(&self, source_chain_id: Id, utxo_id: Id) -> Option<Utxo>;

    /// `BuilderBackend.Balance` — the EVM account balance, in wei.
    fn balance(&self, addr: &[u8; 20]) -> u128;

    /// `BuilderBackend.Nonce`.
    fn nonce(&self, addr: &[u8; 20]) -> u64;
}

/// An in-memory snapshot backend.
#[derive(Default)]
pub struct WalletBackend {
    utxos: BTreeMap<Id, Vec<Utxo>>,
    balances: BTreeMap<[u8; 20], u128>,
    nonces: BTreeMap<[u8; 20], u64>,
}

impl WalletBackend {
    /// Builds a snapshot from per-source-chain UTXO sets + account state.
    #[must_use]
    pub fn new(
        utxo_sets: BTreeMap<Id, Vec<Utxo>>,
        balances: BTreeMap<[u8; 20], u128>,
        nonces: BTreeMap<[u8; 20], u64>,
    ) -> Self {
        let mut utxos = utxo_sets;
        for set in utxos.values_mut() {
            sort_utxos(set);
        }
        Self {
            utxos,
            balances,
            nonces,
        }
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

    fn balance(&self, addr: &[u8; 20]) -> u128 {
        self.balances.get(addr).copied().unwrap_or_default()
    }

    fn nonce(&self, addr: &[u8; 20]) -> u64 {
        self.nonces.get(addr).copied().unwrap_or_default()
    }
}
