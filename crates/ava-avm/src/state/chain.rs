// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `state.ReadOnlyChain` / `state.Chain` surface (`vms/avm/state/state.go`,
//! specs 09 §5).
//!
//! [`Chain`] is the read+write surface shared by the persisted base
//! [`State`](super::state::State) and the in-memory [`Diff`](super::diff::Diff)
//! overlay: every accepted block carries a `Diff`, and on accept the diff chain
//! is applied down to `State`. [`ReadOnlyChain`] is the read-only subset.
//!
//! ## UTXO representation (as-built, M5.10)
//!
//! The spec sketch types the UTXO surface as `avax::Utxo`. `avax::Utxo` carries
//! an `Arc<dyn State>` fx payload that is not yet codec-serializable in isolation
//! (the fx-registered UTXO handler is a later wave), so M5.10 stores UTXOs as
//! their **opaque codec bytes** ([`UtxoBytes`]) — exactly the cross-chain /
//! shared-memory byte layout that *is* protocol-relevant (specs 09 §5.1),
//! mirroring the P-Chain M4.13 as-built. The typed `avax::Utxo` round-trip is
//! layered on by a later task.

use std::time::SystemTime;

use ava_types::id::Id;
use ava_types::short_id::ShortId;

use crate::error::{Error, Result};

/// The opaque codec bytes of an `avax.UTXO` (the protocol-relevant value layout).
///
/// See the module docs for why M5.10 stores UTXOs as bytes rather than the typed
/// `avax::Utxo`.
pub type UtxoBytes = Vec<u8>;

/// `state.ReadOnlyChain` — the read-only surface over X-Chain state (specs 09 §5).
///
/// Absent keys surface as [`Error::Database`](crate::error::Error) wrapping
/// `database.ErrNotFound`, where a Go method returns `database.ErrNotFound`.
pub trait ReadOnlyChain: Send + Sync {
    /// `GetUTXO` — the opaque codec bytes of the UTXO with input id `utxo_id`.
    ///
    /// # Errors
    /// Returns an error if the UTXO is absent or the read fails.
    fn get_utxo(&self, utxo_id: Id) -> Result<UtxoBytes>;

    /// `GetTx` — the stored signed-tx bytes for `tx_id` (parse via the genesis
    /// codec; specs 09 §5.3).
    ///
    /// # Errors
    /// Returns an error if the tx is absent or the read fails.
    fn get_tx(&self, tx_id: Id) -> Result<Vec<u8>>;

    /// `GetBlockIDAtHeight` — the accepted block id at `height`, if any.
    fn get_block_id_at_height(&self, height: u64) -> Option<Id>;

    /// `GetBlock` — the stored codec bytes of the accepted block `blk_id`.
    ///
    /// # Errors
    /// Returns an error if the block is absent or the read fails.
    fn get_block(&self, blk_id: Id) -> Result<Vec<u8>>;

    /// `GetLastAccepted` — the id of the most-recently accepted block.
    fn get_last_accepted(&self) -> Id;

    /// `GetTimestamp` — the current chain time.
    fn get_timestamp(&self) -> SystemTime;

    /// `avax.UTXOReader.UTXOIDs` — the ids of UTXOs referencing `addr`,
    /// starting strictly after `previous` (pass [`Id::EMPTY`] to start at the
    /// beginning), at most `limit` ids (M8.23b address → UTXO index).
    ///
    /// Ordering note (as-built): ids return in **sorted byte order** (the
    /// flat `addr ++ utxoID` index key layout). Go's `utxoState` keeps a
    /// per-address `linkeddb` whose iteration order is insertion-based; the
    /// ordering is node-local (never on the wire), and the sorted order keeps
    /// pagination deterministic (00 §6.1).
    ///
    /// Only the persisted base [`State`](super::state::State) maintains the
    /// index (Go: `avax.UTXOReader` is implemented by `avax.utxoState`; a
    /// `Diff` has no `UTXOIDs`), so the default is an error.
    ///
    /// # Errors
    /// Returns an error if this state view carries no address index or the
    /// read fails.
    fn utxo_ids(&self, addr: &ShortId, previous: Id, limit: usize) -> Result<Vec<Id>> {
        let _ = (addr, previous, limit);
        Err(Error::Service(
            "address→UTXO index unavailable on this state view".to_owned(),
        ))
    }
}

/// `state.Chain` — the read+write surface over X-Chain state, shared by the
/// persisted [`State`](super::state::State) base and the [`Diff`](super::diff::Diff)
/// overlay (specs 09 §5).
///
/// Mutators that cannot fail take `&mut self` and return `()`.
pub trait Chain: ReadOnlyChain {
    /// `AddUTXO` — record the opaque codec `utxo` bytes under its input `id`.
    fn add_utxo(&mut self, id: Id, utxo: UtxoBytes);

    /// `DeleteUTXO` — remove the UTXO with input `id`.
    fn delete_utxo(&mut self, id: Id);

    /// `AddTx` — store the signed-tx `bytes` under `tx_id`.
    fn add_tx(&mut self, tx_id: Id, bytes: Vec<u8>);

    /// `AddBlock` — store an accepted block's codec `bytes` under `blk_id` and
    /// index its `height → id`.
    fn add_block(&mut self, blk_id: Id, height: u64, bytes: Vec<u8>);

    /// `SetLastAccepted` — record the last-accepted block id (singleton).
    fn set_last_accepted(&mut self, blk_id: Id);

    /// `SetTimestamp` — set the current chain time.
    fn set_timestamp(&mut self, t: SystemTime);
}
