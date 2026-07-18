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
    /// This tx's first log's block-wide index: the total count of logs
    /// emitted by every earlier tx in the same block (0 for the block's
    /// first tx, or any tx whose predecessors emitted no logs). Mirrors
    /// go-ethereum's `core/types.Receipts.DeriveFields`, which walks a
    /// block's receipts in order and stamps each log's `Index` as a running
    /// block-wide counter (`logIndex += 1` per log, carried across tx
    /// boundaries) rather than restarting per tx — `eth_getTransactionReceipt`
    /// (`crate::rpc::eth::EthRpc::get_transaction_receipt`) adds this to a
    /// log's position within [`Self::logs`] to report the true block-wide
    /// `logIndex` coreth/geth clients expect.
    pub first_log_index: u64,
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
            first_log_index: 0,
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

    /// `first_log_index` running-offset arithmetic (the block-wide
    /// `logIndex` semantics `block.rs::index_accepted_receipts` computes):
    /// tx 0 emits 2 logs, tx 1 emits 0, tx 2 emits 3 -> first_log_index is
    /// [0, 2, 2] and the block's total log count is 5. Mirrors go-ethereum
    /// `core/types.Receipts.DeriveFields`'s running `logIndex` counter,
    /// carried across tx boundaries rather than reset per tx.
    #[test]
    fn first_log_index_accumulates_across_txs_block_wide() {
        fn logs(n: usize) -> Vec<Log> {
            (0..n)
                .map(|_| Log::new_unchecked(Address::ZERO, Vec::new(), Bytes::new()))
                .collect()
        }
        let per_tx_log_counts = [2usize, 0, 3];
        let mut running = 0u64;
        let mut first_log_indices = Vec::new();
        for &n in &per_tx_log_counts {
            first_log_indices.push(running);
            running = running
                .checked_add(u64::try_from(n).expect("log count fits u64"))
                .expect("running log count must not overflow u64");
        }
        assert_eq!(
            first_log_indices,
            vec![0, 2, 2],
            "first_log_index is the running block-wide log count BEFORE this tx's own logs"
        );
        assert_eq!(running, 5, "block-wide total log count across all 3 txs");

        // Sanity: a record built with the computed offset reports the
        // expected block-wide logIndex for each of its own logs (offset +
        // local position), matching what `EthRpc::get_transaction_receipt`
        // computes.
        let record = TxReceiptRecord {
            tx_hash: B256::repeat_byte(0x01),
            block_hash: B256::repeat_byte(0x02),
            block_number: 7,
            tx_index: 2,
            from: Address::repeat_byte(0x03),
            to: None,
            contract_address: Some(Address::repeat_byte(0x04)),
            gas_used: 50_000,
            cumulative_gas_used: 90_000,
            effective_gas_price: 1,
            success: true,
            logs: logs(3),
            tx_type: 0,
            first_log_index: first_log_indices[2],
        };
        let block_wide_indices: Vec<u64> = (0..record.logs.len())
            .map(|i| record.first_log_index + u64::try_from(i).expect("fits u64"))
            .collect();
        assert_eq!(
            block_wide_indices,
            vec![2, 3, 4],
            "tx 2's 3 logs must report block-wide logIndex 2,3,4 (after tx 0's 2 logs)"
        );
    }
}
