// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Ethereum-state-root view over a firewood database (M1.21, spec 04 §4.1
//! ethhash, 15 §6). Only compiled with the `firewood-ethhash` feature, which
//! globally switches firewood to Keccak-256 + Ethereum-MPT/RLP hashing.
//!
//! Firewood in ethhash mode computes the **EVM state root** over a trie whose
//! keys are account-trie paths (`keccak256(address)` at account depth, slot
//! paths under each account) and whose values are RLP-encoded account/slot data.
//! [`EthHashDb`] is a thin view that takes such [`BatchOp`]s and returns the
//! resulting root via `propose().root_hash()` — the same value the Go node's
//! `firewood-go-ethhash` produces for an identical batch (proven by the golden
//! vector in `tests/golden_firewood_ethhash.rs`).
//!
//! Adapting reth/revm's `StateProvider` onto this view (the full EVM execution
//! integration) is M-EVM scope (spec 04 §4.3); here we only establish root
//! parity.

use ava_types::id::Id;

use crate::firewood::{BatchOp, FirewoodDb, FirewoodProposal, FirewoodResult, empty_root};

/// An Ethereum-state-root view over a firewood database (ethhash mode).
///
/// Wraps a [`FirewoodDb`] opened in Keccak/Eth-MPT mode. Use
/// [`EthHashDb::state_root`] to compute the EVM state root for a batch *without*
/// committing, or [`EthHashDb::commit`] to apply and advance the tip.
pub struct EthHashDb {
    inner: FirewoodDb,
}

impl EthHashDb {
    /// Opens (creating if missing) an ethhash firewood database at `dir`.
    ///
    /// # Errors
    /// Returns a [`FirewoodError`](crate::firewood::FirewoodError) on any
    /// firewood open/config failure.
    pub fn open(dir: impl AsRef<std::path::Path>) -> FirewoodResult<EthHashDb> {
        Ok(EthHashDb {
            inner: FirewoodDb::open(dir)?,
        })
    }

    /// The current committed EVM state root (the Ethereum empty-trie root
    /// `0x56e81f17…` when no state is committed).
    #[must_use]
    pub fn root(&self) -> Id {
        self.inner.root()
    }

    /// The Ethereum empty-trie root for this (ethhash) mode (`0x56e81f17…`),
    /// equal to go-ethereum's `types.EmptyRootHash`.
    #[must_use]
    pub fn empty_root() -> Id {
        empty_root()
    }

    /// Computes the EVM state root that applying `ops` would produce, **without**
    /// committing (consensus votes on this root before commit, 04 §4.2).
    ///
    /// `ops` are RLP-account / storage-slot [`BatchOp`]s (key = trie path, value
    /// = RLP-encoded leaf data).
    ///
    /// # Errors
    /// Returns a [`FirewoodError`](crate::firewood::FirewoodError) if firewood
    /// cannot build the proposal.
    pub fn state_root(&self, ops: Vec<BatchOp>) -> FirewoodResult<Id> {
        Ok(self.inner.propose(ops)?.root())
    }

    /// Builds an (uncommitted) proposal over `ops`, exposing both its root and a
    /// `commit()` handle.
    ///
    /// # Errors
    /// Returns a [`FirewoodError`](crate::firewood::FirewoodError) on a firewood
    /// proposal failure.
    pub fn propose(&self, ops: Vec<BatchOp>) -> FirewoodResult<FirewoodProposal<'_>> {
        self.inner.propose(ops)
    }

    /// Applies `ops` and commits, advancing the tip; returns the new state root.
    ///
    /// # Errors
    /// Returns a [`FirewoodError`](crate::firewood::FirewoodError) on a firewood
    /// proposal/commit failure.
    pub fn commit(&self, ops: Vec<BatchOp>) -> FirewoodResult<Id> {
        let proposal = self.inner.propose(ops)?;
        let root = proposal.root();
        proposal.commit()?;
        Ok(root)
    }

    /// Borrows the underlying firewood database.
    #[must_use]
    pub fn inner(&self) -> &FirewoodDb {
        &self.inner
    }
}
