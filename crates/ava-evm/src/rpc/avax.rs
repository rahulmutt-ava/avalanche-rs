// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `avax.*` JSON-RPC handlers (the C-Chain's Avalanche-network service) + the
//! node health endpoint (G8, spec 10 §9.2/§17.9, M6.24).
//!
//! # Scoping — direct handlers, NOT a jsonrpsee server (the M6.23 precedent)
//!
//! Spec §9.2 sketches "a custom RPC module (axum/JSON-RPC 2.0 …); reth's RPC uses
//! `jsonrpsee` — we mount our module alongside or via `ava-api`'s router". The
//! **mount topology** (jsonrpsee-vs-axum, where the router lives) is explicitly
//! deferred to the 12-node milestone (§9.2, "§12-node"). So — exactly like the
//! `eth_*` handlers ([`crate::rpc::eth`], M6.23) and the avm/platformvm services —
//! [`AvaxRpc`] is a plain handler struct whose methods take typed args and return
//! a [`serde_json::Value`] in coreth's `avax` service JSON shapes
//! (`plugin/evm/atomic/vm/api.go`). Wiring these into a concrete transport (the
//! jsonrpsee-vs-axum decision) is the 12-node task.
//!
//! # Methods (coreth `AvaxAPI`, `plugin/evm/atomic/vm/api.go`)
//!
//! - [`AvaxRpc::issue_tx`] — `avax.issueTx`: decode + parse an atomic tx, add it
//!   to the [`AtomicMempool`] as a local tx, return `{txID}`.
//! - [`AvaxRpc::get_atomic_tx_status`] — `avax.getAtomicTxStatus`:
//!   `{status, blockHeight?}` over the accepted-tx index + the mempool
//!   (`Accepted` > `Processing`/`Dropped` > `Unknown`).
//! - [`AvaxRpc::get_atomic_tx`] — `avax.getAtomicTx`: the signed bytes of an
//!   accepted/processing tx as checksummed hex, `{tx, encoding, blockHeight?}`.
//! - [`AvaxRpc::get_utxos`] — `avax.getUTXOs`: the paginated atomic-UTXO reply
//!   (`{numFetched, utxos, endIndex, encoding}`). The shared-memory indexed UTXO
//!   fetch (`avax.GetAtomicUTXOs`) is **deferred**: `ava-vm`'s `SharedMemory`
//!   does not yet expose the address-indexed iterator coreth reads, so this
//!   returns the empty paginated shape (documented below + in the provenance).
//! - [`AvaxRpc::get_block_by_height`] — `avax.getBlockByHeight`: the canonical
//!   block bytes at a height (over [`CanonicalStore`]), `{block, encoding}`.
//! - [`AvaxRpc::health_check`] — the node health endpoint (coreth `health.go`
//!   `HealthCheck` returns `(nil, nil)` → healthy).

use std::collections::HashMap;
use std::sync::Arc;

use ava_types::id::Id;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::atomic::mempool::{AtomicMempool, MempoolError};
use crate::atomic::tx::Tx;
use crate::canonical::CanonicalStore;
use crate::error::{Error, Result};

/// `maxGetUTXOsAddrs` — the max addresses `avax.getUTXOs` accepts (coreth
/// `api.go:33`).
pub const MAX_GET_UTXOS_ADDRS: usize = 1024;

/// `maxUTXOsToFetch` — the max UTXOs `avax.getUTXOs` returns per call (coreth
/// `api.go:34`).
pub const MAX_UTXOS_TO_FETCH: u32 = 1024;

// ─── Accepted-atomic-tx index (coreth `atomicstate.AtomicRepository`) ──────────

/// The accepted-atomic-tx index: `txID -> (signed bytes, block height)`
/// (coreth `plugin/evm/atomic/state.AtomicRepository.GetByTxID`).
///
/// coreth threads this through the VM's atomic backend; the durable index is
/// advanced on block accept. Until the VM (M6.10 `EvmVm`) wires acceptance into
/// a durable repository, this in-memory index is the seam the handlers read.
/// `put` is the accept-side writer; the handlers only read.
#[derive(Debug, Default)]
pub struct AcceptedAtomicTxIndex {
    /// `txID -> (signed tx bytes, accepted height)`.
    by_id: Mutex<HashMap<Id, (Vec<u8>, u64)>>,
}

impl AcceptedAtomicTxIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `tx_id` as accepted at `height` with the given signed `bytes`
    /// (the accept-side writer; coreth `AtomicRepository.Write`).
    pub fn put(&self, tx_id: Id, bytes: Vec<u8>, height: u64) {
        self.by_id.lock().insert(tx_id, (bytes, height));
    }

    /// `GetByTxID` — the accepted signed bytes + height for `tx_id`, or `None`.
    #[must_use]
    pub fn get(&self, tx_id: &Id) -> Option<(Vec<u8>, u64)> {
        self.by_id.lock().get(tx_id).cloned()
    }
}

// ─── Request args ──────────────────────────────────────────────────────────────

/// `api.FormattedTx` — the `avax.issueTx` request (the encoded signed tx + its
/// encoding name).
#[derive(Clone, Debug, Default)]
pub struct IssueTxArgs {
    /// The encoded signed atomic tx (e.g. `0x…` checksummed hex).
    pub tx: String,
    /// The encoding name (`hex` / `hexc` / `hexnc`). Empty defaults to `hex`.
    pub encoding: String,
}

// ─── The atomic-tx status (coreth `atomic.Status`) ─────────────────────────────

/// `atomic.Status` — the lifecycle state an `avax.getAtomicTxStatus` reports
/// (coreth `plugin/evm/atomic/status.go`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicTxStatus {
    /// `Unknown` — the tx is not known to this node.
    Unknown,
    /// `Dropped` — the tx was in the mempool but dropped (failed verification).
    Dropped,
    /// `Processing` — the tx is in the mempool.
    Processing,
    /// `Accepted` — the tx was accepted into a block.
    Accepted,
}

impl AtomicTxStatus {
    /// The Go `String()` rendering (also the JSON form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AtomicTxStatus::Unknown => "Unknown",
            AtomicTxStatus::Dropped => "Dropped",
            AtomicTxStatus::Processing => "Processing",
            AtomicTxStatus::Accepted => "Accepted",
        }
    }
}

// ─── Handler ───────────────────────────────────────────────────────────────────

/// The `avax.*` RPC handler set (M6.24) + the node health endpoint. Reads /
/// writes the atomic [`AtomicMempool`] (issueTx + processing/dropped lookup), the
/// [`AcceptedAtomicTxIndex`] (accepted lookup), and the [`CanonicalStore`]
/// (getBlockByHeight). Cheaply cloneable (`Arc`-backed).
#[derive(Clone)]
pub struct AvaxRpc {
    /// The atomic X<->C mempool (`avax.issueTx` adds here; status reads it).
    mempool: Arc<Mutex<AtomicMempool>>,
    /// The accepted-block metadata store (`avax.getBlockByHeight`).
    canonical: Arc<CanonicalStore>,
    /// The accepted-atomic-tx index (`avax.getAtomicTx`/`getAtomicTxStatus`).
    accepted: Arc<AcceptedAtomicTxIndex>,
}

impl AvaxRpc {
    /// Builds the handler over the mempool / canonical store / accepted index.
    #[must_use]
    pub fn new(
        mempool: Arc<Mutex<AtomicMempool>>,
        canonical: Arc<CanonicalStore>,
        accepted: Arc<AcceptedAtomicTxIndex>,
    ) -> Self {
        Self {
            mempool,
            canonical,
            accepted,
        }
    }

    // ─── avax.issueTx ──────────────────────────────────────────────────────────

    /// `avax.issueTx` — decode + parse the atomic tx, add it to the mempool as a
    /// **local** tx, and return `{txID}` (the CB58 atomic tx id). An already-known
    /// tx is treated as success (coreth re-pushes it to gossip; the gossip push
    /// is the deferred p2p transport's job).
    ///
    /// # Errors
    /// Returns an error if the bytes fail to decode/parse, or the mempool rejects
    /// the tx for a reason other than already-known.
    pub fn issue_tx(&self, args: IssueTxArgs) -> Result<Value> {
        let raw = decode_formatted_bytes(&args.tx, &args.encoding)?;
        let tx = Tx::parse(&raw)
            .map_err(|e| Error::GenesisParse(format!("problem parsing transaction: {e}")))?;
        let tx_id = tx.id();

        match self.mempool.lock().add_local(tx) {
            Ok(()) => {}
            // coreth: an already-known tx is not an error (it re-pushes to gossip).
            Err(MempoolError::AlreadyKnown) => {}
            Err(e) => return Err(map_mempool_err(e)),
        }
        // (coreth `PushGossiper.Add(tx)` — gossip push is deferred to the p2p
        // transport, see the atomic mempool's gossip seam.)
        Ok(json!({ "txID": tx_id.to_string() }))
    }

    // ─── avax.getAtomicTxStatus ──────────────────────────────────────────────────

    /// `avax.getAtomicTxStatus` — `{status, blockHeight?}`. Accepted (with a
    /// `blockHeight`) takes precedence over the mempool states; a tx the mempool
    /// has is `Processing`, a discarded tx is `Dropped`, otherwise `Unknown`
    /// (coreth `getAtomicTx`).
    ///
    /// # Errors
    /// Returns [`Error::NilTx`] if `tx_id` is the empty id (coreth `errNilTxID`).
    pub fn get_atomic_tx_status(&self, tx_id: Id) -> Result<Value> {
        if tx_id == Id::EMPTY {
            return Err(Error::NilTx);
        }
        let (status, height) = self.lookup_status(tx_id);
        let mut out = json!({ "status": status.as_str() });
        if let Some(h) = height {
            // `json.Uint64` is a quoted decimal string.
            out["blockHeight"] = Value::String(h.to_string());
        }
        Ok(out)
    }

    // ─── avax.getAtomicTx ────────────────────────────────────────────────────────

    /// `avax.getAtomicTx` — `{tx, encoding, blockHeight?}`: the signed bytes of an
    /// accepted/processing tx encoded with `encoding` (default `hex`). An unknown
    /// tx is an error (coreth `could not find tx <id>`).
    ///
    /// # Errors
    /// Returns [`Error::NilTx`] for the empty id, or [`Error::GenesisParse`]
    /// (the string-carrying variant) if the tx is unknown.
    pub fn get_atomic_tx(&self, tx_id: Id, encoding: String) -> Result<Value> {
        if tx_id == Id::EMPTY {
            return Err(Error::NilTx);
        }
        // Accepted (durable index) → its signed bytes + height.
        if let Some((bytes, height)) = self.accepted.get(&tx_id) {
            return Ok(json!({
                "tx": encode_formatted_bytes(&bytes, &encoding)?,
                "encoding": normalize_encoding(&encoding),
                "blockHeight": height.to_string(),
            }));
        }
        // Processing (mempool) → its signed bytes, no height.
        if let Some(bytes) = self.mempool.lock().get_tx_bytes(&tx_id) {
            return Ok(json!({
                "tx": encode_formatted_bytes(&bytes, &encoding)?,
                "encoding": normalize_encoding(&encoding),
            }));
        }
        Err(Error::GenesisParse(format!("could not find tx {tx_id}")))
    }

    // ─── avax.getUTXOs (deferred shared-memory fetch) ────────────────────────────

    /// `avax.getUTXOs` — the paginated atomic-UTXO reply
    /// (`{numFetched, utxos, endIndex, encoding}`).
    ///
    /// **Deferred fetch:** coreth reads `avax.GetAtomicUTXOs` (an address-indexed
    /// shared-memory iterator). `ava-vm`'s `SharedMemory` does not yet expose that
    /// indexed iterator, so this validates the args (address count, encoding) and
    /// returns the **empty** paginated shape — the same envelope coreth returns
    /// when no UTXOs match. Wiring the indexed fetch lands with the shared-memory
    /// iterator (see the provenance note).
    ///
    /// # Errors
    /// Returns an error if `addresses` is empty or exceeds [`MAX_GET_UTXOS_ADDRS`].
    pub fn get_utxos(
        &self,
        addresses: &[String],
        _source_chain: &str,
        limit: u32,
    ) -> Result<Value> {
        if addresses.is_empty() {
            return Err(Error::GenesisParse("no addresses provided".to_string()));
        }
        if addresses.len() > MAX_GET_UTXOS_ADDRS {
            return Err(Error::GenesisParse(format!(
                "number of addresses given, {}, exceeds maximum, {MAX_GET_UTXOS_ADDRS}",
                addresses.len()
            )));
        }
        let _limit = if limit == 0 || limit > MAX_UTXOS_TO_FETCH {
            MAX_UTXOS_TO_FETCH
        } else {
            limit
        };
        // Empty paginated reply (the deferred indexed fetch returns no UTXOs).
        Ok(json!({
            "numFetched": "0",
            "utxos": Value::Array(Vec::new()),
            "endIndex": { "address": "", "utxo": Id::EMPTY.to_string() },
            "encoding": "hex",
        }))
    }

    // ─── avax.getBlockByHeight ───────────────────────────────────────────────────

    /// `avax.getBlockByHeight` — `{block, encoding}`: the canonical block bytes at
    /// `height` from the [`CanonicalStore`], encoded with `encoding` (default
    /// `hex`). A height above the accepted tip / with no stored body is an error.
    ///
    /// # Errors
    /// Returns an error if the canonical read fails or no block exists at
    /// `height`.
    pub fn get_block_by_height(&self, height: u64, encoding: String) -> Result<Value> {
        let bytes = self
            .canonical
            .body_at(height)?
            .ok_or_else(|| Error::GenesisParse(format!("block at height {height} not found")))?;
        Ok(json!({
            "block": encode_formatted_bytes(&bytes, &encoding)?,
            "encoding": normalize_encoding(&encoding),
        }))
    }

    // ─── health ──────────────────────────────────────────────────────────────────

    /// The node health endpoint (coreth `health.go` `HealthCheck` returns
    /// `(nil, nil)` — healthy with no details). Reports `{healthy: true}` plus the
    /// current accepted tip (a useful detail; coreth returns `nil` details today).
    #[must_use]
    pub fn health_check(&self) -> Value {
        let tip = self.canonical.last_canonical().ok().flatten();
        json!({
            "healthy": true,
            "lastAcceptedHeight": tip.map_or_else(|| "0".to_string(), |h| h.to_string()),
        })
    }

    // ─── internals ─────────────────────────────────────────────────────────────

    /// Resolves the `(status, height?)` for `tx_id` (coreth `getAtomicTx`):
    /// Accepted (with height) wins, then Processing / Dropped from the mempool,
    /// else Unknown.
    fn lookup_status(&self, tx_id: Id) -> (AtomicTxStatus, Option<u64>) {
        if let Some((_bytes, height)) = self.accepted.get(&tx_id) {
            return (AtomicTxStatus::Accepted, Some(height));
        }
        let mempool = self.mempool.lock();
        if mempool.has(&tx_id) {
            (AtomicTxStatus::Processing, None)
        } else if mempool.is_discarded(&tx_id) {
            (AtomicTxStatus::Dropped, None)
        } else {
            (AtomicTxStatus::Unknown, None)
        }
    }
}

/// Maps an [`AtomicMempool`] admission error to the C-Chain [`Error`] model
/// (coreth surfaces the mempool error directly from `issueTx`).
fn map_mempool_err(e: MempoolError) -> Error {
    match e {
        MempoolError::NoGasUsed => Error::NoGasUsed,
        MempoolError::Overflow => Error::FeeOverflow,
        MempoolError::Conflict => Error::ConflictingAtomicInputs,
        other => Error::GenesisParse(other.to_string()),
    }
}

// ─── Encoding helpers (coreth `formatting.{Encode,Decode}`) ─────────────────────

/// `formatting.Encode(encoding, bytes)` — the checksummed-hex (`hex`/`hexc`) or
/// no-checksum-hex (`hexnc`) `0x…` form. Mirrors `ava-avm`'s service helper.
fn encode_formatted_bytes(bytes: &[u8], encoding: &str) -> Result<String> {
    match encoding.to_lowercase().as_str() {
        "hex" | "hexc" | "" => {
            let cs = ava_crypto::hashing::checksum(bytes, 4);
            let mut combined = bytes.to_vec();
            combined.extend_from_slice(&cs);
            Ok(format!("0x{}", hex::encode(&combined)))
        }
        "hexnc" => Ok(format!("0x{}", hex::encode(bytes))),
        other => Err(Error::GenesisParse(format!(
            "unsupported encoding {other:?}"
        ))),
    }
}

/// `formatting.Decode(encoding, s)` — the inverse of [`encode_formatted_bytes`]
/// (verifies + strips the 4-byte checksum for `hex`/`hexc`). Mirrors `ava-avm`.
fn decode_formatted_bytes(s: &str, encoding: &str) -> Result<Vec<u8>> {
    let strip_0x = |s: &str| -> Result<Vec<u8>> {
        if !s.starts_with("0x") && !s.starts_with("0X") {
            return Err(Error::GenesisParse(
                "hex decode: missing 0x prefix".to_string(),
            ));
        }
        let hex_str = s.trim_start_matches("0x").trim_start_matches("0X");
        hex::decode(hex_str).map_err(|e| Error::GenesisParse(format!("hex decode: {e}")))
    };
    match encoding.to_lowercase().as_str() {
        "hex" | "hexc" | "" => {
            let decoded = strip_0x(s)?;
            let split_at = decoded.len().checked_sub(4).ok_or_else(|| {
                Error::GenesisParse("hex decode: input too short for checksum".to_string())
            })?;
            let (raw, cs) = decoded.split_at(split_at);
            let expected = ava_crypto::hashing::checksum(raw, 4);
            if cs != expected.as_slice() {
                return Err(Error::GenesisParse(
                    "hex decode: invalid checksum".to_string(),
                ));
            }
            Ok(raw.to_vec())
        }
        "hexnc" => strip_0x(s),
        other => Err(Error::GenesisParse(format!(
            "unsupported encoding {other:?}"
        ))),
    }
}

/// The encoding name echoed in the reply (`formatting.Encoding.String()`); empty
/// defaults to `hex`.
fn normalize_encoding(encoding: &str) -> Value {
    let e = encoding.to_lowercase();
    Value::String(if e.is_empty() { "hex".to_string() } else { e })
}

#[cfg(test)]
mod tests {
    use ava_avm::txs::components::{Output as FxOutput, TransferableOutput};
    use ava_database::MemDb;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};
    use ava_types::short_id::ShortId;

    use super::*;
    use crate::atomic::tx::{AtomicTx, EvmInput, UnsignedExportTx};
    use crate::evmconfig::AvaNextBlockCtx;

    fn id32(b: u8) -> Id {
        Id::from([b; 32])
    }

    /// A signed golden export atomic tx (matches the integration-test fixture).
    fn golden_tx() -> Tx {
        let unsigned = UnsignedExportTx {
            network_id: 1,
            blockchain_id: id32(0x11),
            destination_chain: id32(0x33),
            ins: vec![EvmInput {
                address: [0x02; 20],
                amount: 3000,
                asset_id: id32(0xAA),
                nonce: 7,
            }],
            exported_outputs: vec![TransferableOutput {
                asset_id: id32(0xAA),
                out: FxOutput::SecpTransfer(TransferOutput {
                    amt: 3000,
                    owners: OutputOwners {
                        locktime: 0,
                        threshold: 1,
                        addrs: vec![ShortId::from([0x05; 20])],
                    },
                }),
            }],
        };
        let mut tx = Tx::new(AtomicTx::Export(unsigned));
        tx.initialize().expect("initialize");
        tx
    }

    fn setup() -> (AvaxRpc, Arc<Mutex<AtomicMempool>>) {
        let mempool = Arc::new(Mutex::new(AtomicMempool::new(100, id32(0xAA))));
        let canon_db: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
        let canonical = Arc::new(CanonicalStore::new(canon_db));
        let accepted = Arc::new(AcceptedAtomicTxIndex::new());
        let rpc = AvaxRpc::new(Arc::clone(&mempool), canonical, accepted);
        (rpc, mempool)
    }

    #[test]
    fn get_atomic_tx_status_reports_dropped_for_a_discarded_tx() {
        // A tx that was pulled Current then discarded (failed verification) reads
        // back as `Dropped` (coreth `getAtomicTx` → mempool found+dropped).
        let (rpc, mempool) = setup();
        let tx = golden_tx();
        let tx_id = tx.id();
        {
            let mut mp = mempool.lock();
            mp.add_local(tx).expect("add");
            // Pull the tx Current, then discard it.
            let batch = mp.next_batch(&AvaNextBlockCtx {
                atomic_gas_limit: u64::MAX,
                ..AvaNextBlockCtx::default()
            });
            assert_eq!(batch.len(), 1);
            mp.discard_current_tx(&tx_id);
            assert!(mp.is_discarded(&tx_id));
        }
        let st = rpc.get_atomic_tx_status(tx_id).expect("status");
        assert_eq!(st["status"], Value::String("Dropped".to_string()));
        assert!(st.get("blockHeight").is_none());
    }

    #[test]
    fn get_atomic_tx_status_nil_txid_is_error() {
        let (rpc, _mp) = setup();
        assert!(rpc.get_atomic_tx_status(Id::EMPTY).is_err());
    }

    #[test]
    fn status_strings_match_go() {
        assert_eq!(AtomicTxStatus::Unknown.as_str(), "Unknown");
        assert_eq!(AtomicTxStatus::Dropped.as_str(), "Dropped");
        assert_eq!(AtomicTxStatus::Processing.as_str(), "Processing");
        assert_eq!(AtomicTxStatus::Accepted.as_str(), "Accepted");
    }

    #[test]
    fn hex_encode_decode_roundtrips_with_checksum() {
        let bytes = vec![0x01, 0x02, 0x03, 0xff];
        let enc = encode_formatted_bytes(&bytes, "hex").expect("encode");
        assert!(enc.starts_with("0x"));
        let dec = decode_formatted_bytes(&enc, "hex").expect("decode");
        assert_eq!(dec, bytes);
        // A corrupted checksum is rejected.
        let mut bad = enc.into_bytes();
        let last = bad.len() - 1;
        bad[last] = if bad[last] == b'0' { b'1' } else { b'0' };
        let bad = String::from_utf8(bad).expect("utf8");
        assert!(decode_formatted_bytes(&bad, "hex").is_err());
    }

    #[test]
    fn hexnc_skips_checksum() {
        let bytes = vec![0xaa, 0xbb];
        let enc = encode_formatted_bytes(&bytes, "hexnc").expect("encode");
        assert_eq!(enc, "0xaabb");
        assert_eq!(
            decode_formatted_bytes(&enc, "hexnc").expect("decode"),
            bytes
        );
    }
}
