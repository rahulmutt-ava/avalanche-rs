// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avax` JSON-RPC service mounted at `/avax` (specs/11 §8 — `cchain/api.go`).
//!
//! Port of `vms/saevm/cchain/api.go`'s server-side `service`: it ingests Export
//! and Import transactions (`avax.issueTx`) and serves accepted cross-chain txs
//! (`avax.getAtomicTx`) and exported UTXOs (`avax.getUTXOs`).
//!
//! # RPC convention (specs/11 §13; project precedent)
//!
//! All SAE RPC is a **direct `serde_json` handler**, not jsonrpsee — matching
//! [`ava_saevm_core::rpc`] (M7.19). [`AvaxService`] therefore exposes typed
//! in-process methods ([`AvaxService::issue_tx`], [`AvaxService::get_atomic_tx`])
//! plus a thin [`AvaxService::handle`] dispatcher over a
//! [`serde_json::Value`] envelope; the live HTTP transport (gorilla RPC in Go)
//! is mounted by the node assembly (M8) and is not needed for the in-process
//! smoke this task delivers.
//!
//! # AS-BUILT deviations
//!
//! * **Gossip.** Go's `IssueTx` adds to a bloom-`gossipSet` and a `pushGossiper`;
//!   the Rust gossip/p2p seam (`txgossip`, M7.20) is not yet wired into the VM,
//!   so `issue_tx` admits the tx directly into the [`AtomicTxpool`] (the local
//!   side of Go's behaviour) and the push-gossip is a `// TODO(M7.x)` follow-up.
//! * **`getUTXOs` address formatting.** Go formats bech32 addresses via
//!   `snow.Context.BCLookup`/`constants.GetHRP`; that node-context machinery is
//!   M8. The Rust [`AvaxService::get_utxos`] queries shared memory directly by
//!   raw 20-byte address + source chain (the consensus-critical read), leaving
//!   bech32 formatting / pagination-cursor encoding as `// TODO(M8)`.

use std::sync::Arc;

use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::SharedMemory;
use parking_lot::Mutex;

use crate::state::State;
use crate::tx::Tx;
use crate::txpool::AtomicTxpool;

/// The `avax` service name (Go `avaxServiceName`).
pub const AVAX_SERVICE_NAME: &str = "avax";

/// The HTTP extension path the `avax` service is mounted at, `/avax` (Go
/// `avaxHTTPExtensionPath`).
pub const AVAX_EXTENSION_PATH: &str = "/avax";

/// The maximum number of UTXOs returned by a single `getUTXOs` call (Go
/// `maxGetUTXOsLimit`).
pub const MAX_GET_UTXOS_LIMIT: usize = 1024;

/// Errors returned by the `avax` service.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A tx could not be admitted into the pool for a reason other than being
    /// already known (Go ignores `ErrAlreadyKnown`).
    #[error("issuing tx: {0}")]
    Issuing(crate::txpool::Error),
    /// The requested tx was not found in the accepted-tx index.
    #[error("fetching tx: {0}")]
    Fetching(crate::state::Error),
    /// A shared-memory read failed.
    #[error("retrieving UTXOs: {0}")]
    SharedMemory(ava_vm::error::Error),
    /// A JSON request was malformed (missing/invalid `method` or fields).
    #[error("malformed request: {0}")]
    Malformed(&'static str),
}

/// The server-side handler for the `avax` RPC API (Go `cchain.service`).
///
/// Holds the atomic [`AtomicTxpool`] (`issueTx` admits here), the atomic-tx
/// [`State`] index (`getAtomicTx` reads here), and the C-Chain's shared-memory
/// view (`getUTXOs` reads here).
pub struct AvaxService {
    txpool: Arc<AtomicTxpool>,
    state: Arc<Mutex<State>>,
    shared_memory: Arc<dyn SharedMemory>,
    /// The C-Chain id (the peer chains' UTXOs are read against it).
    chain_id: Id,
}

impl AvaxService {
    /// Constructs the `avax` service (Go `newService`).
    #[must_use]
    pub fn new(
        txpool: Arc<AtomicTxpool>,
        state: Arc<Mutex<State>>,
        shared_memory: Arc<dyn SharedMemory>,
        chain_id: Id,
    ) -> Self {
        Self {
            txpool,
            state,
            shared_memory,
            chain_id,
        }
    }

    /// The C-Chain id this service serves.
    #[must_use]
    pub fn chain_id(&self) -> Id {
        self.chain_id
    }

    /// `avax.issueTx` (Go `service.IssueTx`): admit `tx` into the atomic txpool
    /// and return its id. Re-issuing an already-known tx is a no-op that still
    /// reports the id (Go ignores `txpool.ErrAlreadyKnown`).
    ///
    /// TODO(M7.x): also add to the bloom-gossip set + push gossiper once the
    /// `txgossip` seam is wired into the VM.
    ///
    /// # Errors
    /// [`Error::Issuing`] if the pool rejects the tx for any reason other than
    /// being already known.
    // The `Result` is Go-faithful (`IssueTx` returns an error) and future-proof:
    // M7.x's state-verified `Txpool.Add` + bloom-gossip admission introduce
    // non-`AlreadyKnown` rejections. Today the pool's only error is
    // `AlreadyKnown` (which Go ignores), so the body never takes the error path.
    #[allow(clippy::unnecessary_wraps)]
    pub fn issue_tx(&self, tx: &Tx) -> Result<Id, Error> {
        let id = tx.id();
        match self.txpool.add(tx.clone()) {
            Ok(()) | Err(crate::txpool::Error::AlreadyKnown) => Ok(id),
        }
    }

    /// `avax.getAtomicTx` (Go `service.GetAtomicTx`): the accepted cross-chain tx
    /// with `tx_id` and the block height it was accepted at.
    ///
    /// # Errors
    /// [`Error::Fetching`] if `tx_id` is unknown to the accepted-tx index.
    pub fn get_atomic_tx(&self, tx_id: Id) -> Result<(Tx, u64), Error> {
        self.state.lock().get_tx(tx_id).map_err(Error::Fetching)
    }

    /// `avax.getUTXOs` (Go `service.GetUTXOs`): the raw UTXO bytes controlled by
    /// `addrs` that `source_chain` has exported to the C-Chain, capped at
    /// [`MAX_GET_UTXOS_LIMIT`].
    ///
    /// Returns the indexed UTXO bytes (bech32 address formatting + the
    /// pagination cursor are M8 — see the module AS-BUILT note).
    ///
    /// # Errors
    /// [`Error::SharedMemory`] if the indexed read fails.
    pub fn get_utxos(&self, source_chain: Id, addrs: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Error> {
        let (values, _last_trait, _last_key) = self
            .shared_memory
            .indexed(source_chain, addrs, &[], &[], MAX_GET_UTXOS_LIMIT)
            .map_err(Error::SharedMemory)?;
        Ok(values)
    }

    /// A thin `serde_json` dispatcher over the `avax` methods (the in-process
    /// envelope; the live HTTP transport is M8). Recognises `avax.issueTx`
    /// (echoing the supplied `txID`) and `avax.getAtomicTx`.
    ///
    /// # Errors
    /// [`Error::Malformed`] if `method`/required fields are missing or invalid,
    /// or [`Error::Fetching`] for an unknown `getAtomicTx` id.
    pub fn handle(&self, req: &serde_json::Value) -> Result<serde_json::Value, Error> {
        let method = req
            .get("method")
            .and_then(serde_json::Value::as_str)
            .ok_or(Error::Malformed("missing method"))?;
        match method {
            "avax.issueTx" => {
                let tx_id = req
                    .get("txID")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(Error::Malformed("missing txID"))?;
                Ok(serde_json::json!({ "txID": tx_id }))
            }
            "avax.getAtomicTx" => {
                let tx_id_str = req
                    .get("txID")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(Error::Malformed("missing txID"))?;
                let tx_id: Id = tx_id_str
                    .parse()
                    .map_err(|_| Error::Malformed("invalid txID"))?;
                let (_tx, height) = self.get_atomic_tx(tx_id)?;
                Ok(serde_json::json!({ "txID": tx_id_str, "blockHeight": height }))
            }
            _ => Err(Error::Malformed("unknown method")),
        }
    }
}
