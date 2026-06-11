// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The shared cross-chain UTXO store — port of
//! `wallet/subnet/primary/common/utxos.go`.
//!
//! Go keeps one `common.UTXOs` map keyed `(source chain, destination chain)`
//! shared by all three chain wallets, so a P→X export immediately becomes
//! visible to the X wallet's importable set. The Rust port keeps the same
//! shape but is *typed per destination*: UTXOs destined to the P-chain decode
//! through the platformvm codec ([`ava_platformvm::utxo::Utxo`], which can
//! carry stakeable-lock outputs), while UTXOs destined to the X/C chains use
//! the avm shape ([`ava_avm::txs::executor::semantic::Utxo`]). The two encode
//! byte-identically for the secp256k1fx transfer outputs that cross chains
//! (ATOMIC-1), and [`p_output_to_avm`] / [`avm_output_to_p`] convert at the
//! boundary.
//!
//! Reads return the canonical deterministic order (`UTXOID.Compare`) the
//! builders require — Go iterates a map (random order); the Rust port always
//! sorts (see [`crate::common::utxo_select`]).

use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

use ava_types::id::Id;

use crate::error::{Error, Result};

/// A P-chain (platformvm-typed) UTXO.
pub type PUtxo = ava_platformvm::utxo::Utxo;
/// An X/C-chain (avm-typed) UTXO.
pub type XcUtxo = ava_avm::txs::executor::semantic::Utxo;

#[derive(Default)]
struct Inner {
    /// Destination = P: source chain → `input_id` → UTXO.
    to_p: BTreeMap<Id, BTreeMap<Id, PUtxo>>,
    /// Destination = X/C: destination chain → source chain → `input_id` → UTXO.
    to_xc: BTreeMap<Id, BTreeMap<Id, BTreeMap<Id, XcUtxo>>>,
}

/// `common.NewUTXOs()` — the shared `(source, destination)`-keyed UTXO map.
#[derive(Default)]
pub struct UtxoStore {
    inner: RwLock<Inner>,
}

impl UtxoStore {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// `UTXOs.AddUTXO(source, destination = P, utxo)`.
    pub fn add_p(&self, source_chain_id: Id, utxo: PUtxo) {
        self.write()
            .to_p
            .entry(source_chain_id)
            .or_default()
            .insert(utxo.input_id(), utxo);
    }

    /// `UTXOs.RemoveUTXO(source, destination = P, utxo_id)` — a no-op if the
    /// UTXO is absent (Go returns `nil`).
    pub fn remove_p(&self, source_chain_id: Id, utxo_id: Id) {
        let mut inner = self.write();
        if let Some(set) = inner.to_p.get_mut(&source_chain_id) {
            set.remove(&utxo_id);
            if set.is_empty() {
                inner.to_p.remove(&source_chain_id);
            }
        }
    }

    /// `UTXOs.UTXOs(source, destination = P)` in canonical order.
    #[must_use]
    pub fn p_utxos(&self, source_chain_id: Id) -> Vec<PUtxo> {
        let mut utxos: Vec<PUtxo> = self
            .read()
            .to_p
            .get(&source_chain_id)
            .map(|set| set.values().cloned().collect())
            .unwrap_or_default();
        crate::common::utxo_select::sort_utxos(&mut utxos);
        utxos
    }

    /// `UTXOs.GetUTXO(source, destination = P, utxo_id)`.
    #[must_use]
    pub fn get_p(&self, source_chain_id: Id, utxo_id: Id) -> Option<PUtxo> {
        self.read()
            .to_p
            .get(&source_chain_id)?
            .get(&utxo_id)
            .cloned()
    }

    /// `UTXOs.AddUTXO(source, destination = X/C, utxo)`.
    pub fn add_xc(&self, source_chain_id: Id, destination_chain_id: Id, utxo: XcUtxo) {
        self.write()
            .to_xc
            .entry(destination_chain_id)
            .or_default()
            .entry(source_chain_id)
            .or_default()
            .insert(utxo.input_id(), utxo);
    }

    /// `UTXOs.RemoveUTXO(source, destination = X/C, utxo_id)` — a no-op if the
    /// UTXO is absent.
    pub fn remove_xc(&self, source_chain_id: Id, destination_chain_id: Id, utxo_id: Id) {
        let mut inner = self.write();
        if let Some(by_source) = inner.to_xc.get_mut(&destination_chain_id) {
            if let Some(set) = by_source.get_mut(&source_chain_id) {
                set.remove(&utxo_id);
                if set.is_empty() {
                    by_source.remove(&source_chain_id);
                }
            }
            if by_source.is_empty() {
                inner.to_xc.remove(&destination_chain_id);
            }
        }
    }

    /// `UTXOs.UTXOs(source, destination = X/C)` in canonical order.
    #[must_use]
    pub fn xc_utxos(&self, source_chain_id: Id, destination_chain_id: Id) -> Vec<XcUtxo> {
        let mut utxos: Vec<XcUtxo> = self
            .read()
            .to_xc
            .get(&destination_chain_id)
            .and_then(|by_source| by_source.get(&source_chain_id))
            .map(|set| set.values().cloned().collect())
            .unwrap_or_default();
        crate::x::backend::sort_utxos(&mut utxos);
        utxos
    }

    /// `UTXOs.GetUTXO(source, destination = X/C, utxo_id)`.
    #[must_use]
    pub fn get_xc(
        &self,
        source_chain_id: Id,
        destination_chain_id: Id,
        utxo_id: Id,
    ) -> Option<XcUtxo> {
        self.read()
            .to_xc
            .get(&destination_chain_id)?
            .get(&source_chain_id)?
            .get(&utxo_id)
            .cloned()
    }
}

/// Converts a P-chain fx output into the avm shape (a P→X/C export crossing
/// the typed boundary). Only `secp256k1fx.TransferOutput` crosses chains.
///
/// # Errors
/// [`Error::UnknownOutputType`] for a stakeable-lock output (never exported by
/// the wallet builders).
pub fn p_output_to_avm(
    out: &ava_platformvm::txs::components::Output,
) -> Result<ava_avm::txs::components::Output> {
    match out {
        ava_platformvm::txs::components::Output::Transfer(o) => {
            Ok(ava_avm::txs::components::Output::SecpTransfer(o.clone()))
        }
        ava_platformvm::txs::components::Output::StakeableLock(_) => Err(Error::UnknownOutputType),
    }
}

/// Converts an avm fx output into the P-chain shape (an X/C→P export crossing
/// the typed boundary). Only `secp256k1fx.TransferOutput` crosses chains.
///
/// # Errors
/// [`Error::UnknownOutputType`] for a mint output (never exported by the
/// wallet builders).
pub fn avm_output_to_p(
    out: &ava_avm::txs::components::Output,
) -> Result<ava_platformvm::txs::components::Output> {
    match out {
        ava_avm::txs::components::Output::SecpTransfer(o) => {
            Ok(ava_platformvm::txs::components::Output::Transfer(o.clone()))
        }
        ava_avm::txs::components::Output::SecpMint(_) => Err(Error::UnknownOutputType),
    }
}
