// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Accepted-tx receipts: the verify-time stash, the accept-time persisted
//! encoding, and the `AcceptedTxIndex` Task 4's RPC layer reads
//! (cchain-tx-pipeline design doc, task 3).
//!
//! `EvmBlock::verify` (`crate::block`) executes txs via
//! [`ava_evm_reth::ExternalConsensusExecutor::execute_batch`], whose
//! `ExecOutcome::result.receipts` is the ONLY place these receipts exist —
//! `accept` cannot re-derive them without re-executing the block. So `verify`
//! stashes them (keyed by pre-commit root, the same warp-seam idiom
//! `crate::block::EvmBlockContext` already uses for `SendWarpMessage` logs);
//! `accept` takes the stash, persists the encoded bytes into
//! [`crate::canonical::CanonicalStore`]'s `RECEIPTS` column, writes a
//! `tx_hash -> block number` row per tx (`CanonicalStore::put_tx_number`), and
//! records a [`TxReceiptRecord`] per tx into this module's [`AcceptedTxIndex`].
//! `reject` drops the stash entry, mirroring the warp seam's reject-path
//! cleanup.
//!
//! A missing stash at accept (verify did not run in this process — e.g. an
//! accept-only replay/resume path) is NOT an accept failure: `EvmBlock::accept`
//! persists an empty receipts list and skips indexing, logging at `debug`
//! (the M6.24 placeholder behavior this task replaces for the common case).
//!
//! # Encoding
//!
//! Each accepted receipt is wrapped as its EIP-2718 receipt envelope
//! ([`ReceiptWithBloom::encoded_2718`]) and the block's list of envelopes is
//! RLP-encoded as a `Vec<Bytes>` — one opaque per-tx envelope per slot, `Vec`
//! framing for the block boundary (reth's receipt static file is the same
//! per-tx-envelope shape; we RLP-frame the list ourselves rather than adopting
//! reth's static-file format, matching [`crate::canonical::CanonicalStore`]'s
//! module-level "non-state block metadata only, never reth's on-disk schema"
//! scope note).

use std::collections::HashMap;

use ava_evm_reth::{
    Address, B256, Bytes, Decodable2718, Encodable2718, EthReceipt, Log, ReceiptWithBloom,
    RlpDecodable, RlpEncodable, TxReceipt as _,
};
use parking_lot::Mutex;

use crate::error::{Error, Result};

/// A single accepted tx's receipt, keyed by `tx_hash` — the shape Task 4's
/// `eth_getTransactionReceipt` handler assembles its JSON response from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxReceiptRecord {
    /// The transaction hash.
    pub tx_hash: B256,
    /// The hash of the block this tx was accepted in.
    pub block_hash: B256,
    /// The height of the block this tx was accepted in.
    pub block_number: u64,
    /// This tx's index within the block.
    pub tx_index: u64,
    /// The recovered sender.
    pub from: Address,
    /// The call target, or `None` for a contract-creation tx.
    pub to: Option<Address>,
    /// The created contract's address, for a contract-creation tx (`to ==
    /// None`); `None` otherwise.
    pub contract_address: Option<Address>,
    /// This tx's own gas usage — the block's cumulative gas used through this
    /// tx, minus the cumulative gas used through the previous tx (or the raw
    /// cumulative value for the block's first tx).
    pub gas_used: u64,
    /// The block's cumulative gas used through and including this tx.
    pub cumulative_gas_used: u64,
    /// The effective gas price this tx paid: `gas_price` for a legacy tx,
    /// `min(max_fee_per_gas, base_fee + max_priority_fee_per_gas)` for a
    /// dynamic-fee tx (alloy `Transaction::effective_gas_price`, the same
    /// formula coreth/geth `types.Transaction` computes for
    /// `eth_getTransactionReceipt`'s `effectiveGasPrice`).
    pub effective_gas_price: u128,
    /// Whether the tx succeeded (EIP-658 status).
    pub success: bool,
    /// The logs this tx emitted.
    pub logs: Vec<Log>,
    /// The EIP-2718 tx type byte.
    pub tx_type: u8,
}

/// The accepted-tx index: `tx_hash -> TxReceiptRecord`, the seam Task 4's
/// `eth_getTransactionReceipt` reads (same interior-mutability shape as
/// [`crate::rpc::avax::AcceptedAtomicTxIndex`]). `EvmBlock::accept` is the sole
/// writer (via [`AcceptedTxIndex::record`]).
///
/// # Acknowledged debt: unbounded in-memory growth
///
/// This index retains every accepted tx's [`TxReceiptRecord`] (including its
/// `logs`) for the life of the process — there is no eviction, bounded
/// window, or persistence-backed lookup. This matches the scope of its
/// sibling [`crate::rpc::avax::AcceptedAtomicTxIndex`] (same shape, same
/// gap), so this task does not introduce a new class of problem, but it IS
/// now wired into the live `EvmBlock::accept` path (unlike the atomic
/// sibling, whose accept-side writer is still a documented TODO there), so
/// the growth is real and unbounded on a running node from this task onward.
///
/// TODO(M6.24 receipts/history): replace with a bounded in-memory window
/// (e.g. last-N-blocks) backed by [`crate::canonical::CanonicalStore`]'s
/// persisted `RECEIPTS`/`TX_NUMBER` rows for anything older — the same
/// reth-db history-schema migration already deferred for
/// [`crate::canonical::CanonicalStore`] (see its module doc) is the natural
/// place to add a real lookup path that doesn't require keeping every
/// receipt in memory forever.
#[derive(Debug, Default)]
pub struct AcceptedTxIndex {
    by_hash: Mutex<HashMap<B256, TxReceiptRecord>>,
}

impl AcceptedTxIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a block's accepted tx receipts (the accept-side writer).
    pub fn record(&self, records: Vec<TxReceiptRecord>) {
        let mut guard = self.by_hash.lock();
        for record in records {
            guard.insert(record.tx_hash, record);
        }
    }

    /// The receipt record for `hash`, or `None` if unknown.
    #[must_use]
    pub fn get(&self, hash: &B256) -> Option<TxReceiptRecord> {
        self.by_hash.lock().get(hash).cloned()
    }
}

/// RLP-encodes a block's receipts as `Vec<Bytes>` — each entry the receipt's
/// EIP-2718 envelope (`ReceiptWithBloom::encoded_2718`, alloy's
/// `TxReceipt::into_with_bloom` computing the per-receipt logs bloom). This is
/// the exact bytes [`crate::canonical::CanonicalStore::append_canonical`]
/// persists in the `RECEIPTS` column.
#[must_use]
pub fn encode_block_receipts(receipts: &[EthReceipt]) -> Vec<u8> {
    let envelopes: Vec<Bytes> = receipts
        .iter()
        .cloned()
        .map(|r| Bytes::from(r.into_with_bloom().encoded_2718()))
        .collect();
    let mut out = Vec::new();
    envelopes.encode(&mut out);
    out
}

/// The inverse of [`encode_block_receipts`].
///
/// # Errors
/// Returns [`Error::ReceiptDecode`] if `bytes` is not a valid RLP list of
/// `Bytes`, any entry is not a valid EIP-2718 receipt envelope, or an entry
/// carries trailing bytes after its envelope.
pub fn decode_block_receipts(bytes: &[u8]) -> Result<Vec<ReceiptWithBloom<EthReceipt>>> {
    let mut buf: &[u8] = bytes;
    let envelopes: Vec<Bytes> =
        RlpDecodable::decode(&mut buf).map_err(|e| Error::ReceiptDecode(e.to_string()))?;
    if !buf.is_empty() {
        return Err(Error::ReceiptDecode(
            "trailing bytes after receipts list".to_string(),
        ));
    }
    envelopes
        .iter()
        .map(|envelope| {
            ReceiptWithBloom::<EthReceipt>::decode_2718_exact(envelope.as_ref())
                .map_err(|e| Error::ReceiptDecode(e.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ava_evm_reth::TxType;

    use super::*;

    fn receipt(
        tx_type: TxType,
        success: bool,
        cumulative_gas_used: u64,
        logs: Vec<Log>,
    ) -> EthReceipt {
        EthReceipt {
            tx_type,
            success,
            cumulative_gas_used,
            logs,
        }
    }

    fn sample_log() -> Log {
        Log::new_unchecked(
            Address::repeat_byte(0x11),
            vec![B256::repeat_byte(0x22)],
            Bytes::from_static(b"payload"),
        )
    }

    #[test]
    fn encode_decode_round_trips_logs_and_no_logs() {
        let with_logs = receipt(TxType::Eip1559, true, 21_000, vec![sample_log()]);
        let without_logs = receipt(TxType::Legacy, false, 42_000, Vec::new());
        let receipts = vec![with_logs.clone(), without_logs.clone()];

        let encoded = encode_block_receipts(&receipts);
        assert!(!encoded.is_empty(), "encode_block_receipts");

        let decoded = decode_block_receipts(&encoded).expect("decode_block_receipts");
        assert_eq!(decoded.len(), 2, "decoded receipt count");

        assert_eq!(decoded[0].receipt, with_logs, "receipt 0 round-trip");
        assert_eq!(
            decoded[0].logs_bloom,
            with_logs.into_with_bloom().logs_bloom,
            "receipt 0 bloom round-trip"
        );
        assert_eq!(decoded[1].receipt, without_logs, "receipt 1 round-trip");
    }

    #[test]
    fn decode_empty_receipts_is_empty() {
        let encoded = encode_block_receipts(&[]);
        let decoded = decode_block_receipts(&encoded).expect("decode_block_receipts");
        assert!(decoded.is_empty(), "no receipts decodes to an empty list");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_block_receipts(&[0xff, 0x00]).is_err());
    }

    #[test]
    fn accepted_tx_index_records_and_gets() {
        let index = AcceptedTxIndex::new();
        let hash = B256::repeat_byte(0x42);
        let record = TxReceiptRecord {
            tx_hash: hash,
            block_hash: B256::repeat_byte(0x01),
            block_number: 1,
            tx_index: 0,
            from: Address::repeat_byte(0x02),
            to: Some(Address::repeat_byte(0x03)),
            contract_address: None,
            gas_used: 21_000,
            cumulative_gas_used: 21_000,
            effective_gas_price: 1_000_000_000,
            success: true,
            logs: Vec::new(),
            tx_type: 2,
        };

        assert_eq!(index.get(&hash), None, "unrecorded hash misses");
        index.record(vec![record.clone()]);
        assert_eq!(index.get(&hash), Some(record), "recorded hash hits");
        assert_eq!(
            index.get(&B256::repeat_byte(0x99)),
            None,
            "unknown hash still misses"
        );
    }
}
