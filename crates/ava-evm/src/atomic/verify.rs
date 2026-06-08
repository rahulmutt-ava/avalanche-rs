// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic-tx **semantic verify**: conflict sets and the mainnet `bonusBlocks`
//! skip-set (spec 10 §6.5).
//!
//! Port of coreth's block-level atomic verification:
//! - `plugin/evm/atomic/vm/vm.go` (`verifyTxs`) — the per-block loop that rejects
//!   a block whose atomic txs conflict **with each other** (intra-block) or with
//!   an atomic tx in a **processing ancestor** (up to last-accepted).
//! - `plugin/evm/atomic/vm/tx_semantic_verifier.go` (`conflicts`) — the ancestry
//!   walk that `verifyTxs` delegates to per tx.
//! - `plugin/evm/atomic/import_tx.go` / `export_tx.go` (`InputUTXOs`) — the set of
//!   consumed UTXO ids per tx (the "conflict set" granularity).
//! - `plugin/evm/atomic/vm/bonus_blocks.go` (`readMainnetBonusBlocks`) — the 57
//!   mainnet blocks whose atomic ops are **indexed but skipped** when applying to
//!   shared memory (a historical mainnet repair; provenance below).
//!
//! # Conflict-set semantics (coreth `verifyTxs` + `conflicts`)
//!
//! Each atomic tx consumes a set of UTXO ids ([`input_utxos`]). A block is invalid
//! if any two of its txs share a consumed id (double-spend within the block), or
//! if any tx shares a consumed id with an atomic tx in a still-processing ancestor
//! block (a fork double-spend). Accepted ancestors need not be re-checked — the
//! shared-memory `Get` in the per-tx verify already proves their UTXOs were
//! removed, so coreth's `conflicts` returns as soon as it walks past the
//! last-accepted height. The caller therefore supplies only the union of consumed
//! ids over the **processing** ancestry as `ancestor_inputs`.
//!
//! # `InputUTXOs` id derivation
//!
//! - **Import** (`UnsignedImportTx.InputUTXOs`): each input's
//!   `UTXOID.InputID()` = `tx_id.prefix(output_index)` (sha256 of
//!   `be_u64(output_index) ++ tx_id`).
//! - **Export** (`UnsignedExportTx.InputUTXOs`): each `EVMInput` packs
//!   `Packer{PackLong(nonce); PackBytes(address)}` into a fixed 32-byte buffer:
//!   `nonce` (8 bytes big-endian) ++ `len(address)` (4 bytes big-endian = 20) ++
//!   `address` (20 bytes). The 32 raw bytes are used directly as the id (NOT
//!   hashed).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use ava_types::id::Id;

use crate::atomic::tx::AtomicTx;
use crate::error::{Error, Result};

/// `(UnsignedAtomicTx).InputUTXOs` — the set of UTXO ids `tx` consumes (coreth
/// `import_tx.go` / `export_tx.go`). This is the conflict-set granularity: two
/// txs conflict iff their `input_utxos` sets overlap.
///
/// - Import: each imported input's `InputID()` = `tx_id.prefix(output_index)`.
/// - Export: each `EVMInput`'s packed `(nonce, address)` 32-byte id.
#[must_use]
pub fn input_utxos(tx: &AtomicTx) -> BTreeSet<Id> {
    let mut set = BTreeSet::new();
    match tx {
        AtomicTx::Import(utx) => {
            for input in &utx.imported_inputs {
                set.insert(input.input_id());
            }
        }
        AtomicTx::Export(utx) => {
            for input in &utx.ins {
                set.insert(export_input_id(input.nonce, &input.address));
            }
        }
    }
    set
}

/// `EVMInput` → `ids.ID` (coreth `export_tx.go` `InputUTXOs`):
/// `Packer{PackLong(nonce); PackBytes(address)}` over a 32-byte buffer —
/// `nonce` (8 BE) ++ `len(address)` (4 BE = 20) ++ `address` (20). The packed
/// bytes are used directly as the id (not hashed). Total = 8 + 4 + 20 = 32.
fn export_input_id(nonce: u64, address: &[u8; 20]) -> Id {
    let mut raw = [0u8; 32];
    raw[..8].copy_from_slice(&nonce.to_be_bytes());
    // PackBytes writes a 4-byte big-endian length prefix (the address is always
    // 20 bytes on the C-Chain), then the address bytes.
    raw[8..12].copy_from_slice(&20u32.to_be_bytes());
    raw[12..].copy_from_slice(address);
    Id::from(raw)
}

/// **Semantic verify** the conflict set of a block's atomic `txs` (coreth
/// `verifyTxs`).
///
/// Walks `txs` in order, accumulating their consumed UTXO ids:
/// 1. If a tx's [`input_utxos`] overlaps the ids already consumed by an earlier
///    tx **in the same block**, the block double-spends — return
///    [`Error::ConflictingAtomicInputs`].
/// 2. If a tx's ids overlap `ancestor_inputs` (the union of UTXO ids consumed by
///    atomic txs in the still-**processing** ancestry, back to last-accepted),
///    the block forks a double-spend — return [`Error::ConflictingAtomicInputs`].
///
/// `ancestor_inputs` is empty when the parent is the last-accepted block (the
/// common linear-accept case): coreth's `conflicts` stops at last-accepted, so an
/// accepted parent contributes nothing.
///
/// # Errors
/// Returns [`Error::ConflictingAtomicInputs`] on any intra-block or ancestry
/// conflict.
pub fn verify_no_conflicts(txs: &[AtomicTx], ancestor_inputs: &BTreeSet<Id>) -> Result<()> {
    let mut seen: BTreeSet<Id> = BTreeSet::new();
    for tx in txs {
        let inputs = input_utxos(tx);
        // Conflict with a processing ancestor (coreth `conflicts`).
        if !inputs.is_disjoint(ancestor_inputs) {
            return Err(Error::ConflictingAtomicInputs);
        }
        // Conflict with an earlier tx in this same block (coreth `verifyTxs`
        // `inputs.Overlaps(txInputs)`).
        if !inputs.is_disjoint(&seen) {
            return Err(Error::ConflictingAtomicInputs);
        }
        seen.extend(inputs);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// bonusBlocks (coreth `plugin/evm/atomic/vm/bonus_blocks.go`)
// ---------------------------------------------------------------------------

/// The mainnet bonus-block table verbatim from coreth
/// `plugin/evm/atomic/vm/bonus_blocks.go` `readMainnetBonusBlocks`: `(height,
/// CB58 block id)`. These 57 blocks are still **indexed** in the atomic trie but
/// their atomic ops are **skipped** when applying to shared memory (a historical
/// mainnet repair — coreth `atomic_backend.go`: "If [height] is a bonus block, do
/// not apply the atomic operations to shared memory").
const MAINNET_BONUS_BLOCKS: &[(u64, &str)] = &[
    (102972, "Njm9TcLUXRojZk8YhEM6ksvfiPdC1TME4zJvGaDXgzMCyB6oB"),
    (103105, "BYqLB6xpqy7HsAgP2XNfGE8Ubg1uEzse5mBPTSJH9z5s8pvMa"),
    (103143, "AfWvJH3rB2fdHuPWQp6qYNCFVT29MooQPRigD88rKKwUDEDhq"),
    (103183, "2KPW9G5tiNF14tZNfG4SqHuQrtUYVZyxuof37aZ7AnTKrQdsHn"),
    (103197, "pE93VXY3N5QKfwsEFcM9i59UpPFgeZ8nxpJNaGaDQyDgsscNf"),
    (103203, "2czmtnBS44VCWNRFUM89h4Fe9m3ZeZVYyh7Pe3FhNqjRNgPXhZ"),
    (103208, "esx5J962LtYm2aSrskpLai5e4CMMsaS1dsu9iuLGJ3KWgSu2M"),
    (103209, "DK9NqAJGry1wAo767uuYc1dYXAjUhzwka6vi8d9tNheqzGUTd"),
    (103259, "i1HoerJ1axognkUKKL58FvF9aLrbZKtv7TdKLkT5kgzoeU1vB"),
    (103261, "2DpCuBaH94zKKFNY2XTs4GeJcwsEv6qT2DHc59S8tdg97GZpcJ"),
    (103266, "2ez4CA7w4HHr8SSobHQUAwFgj2giRNjNFUZK9JvrZFa1AuRj6X"),
    (103287, "2QBNMMFJmhVHaGF45GAPszKyj1gK6ToBERRxYvXtM7yfrdUGPK"),
    (103339, "2pSjfo7rkFCfZ2CqAxqfw8vqM2CU2nVLHrFZe3rwxz43gkVuGo"),
    (103346, "2SiSziHHqPjb1qkw7CdGYupokiYpd2b7mMqRiyszurctcA5AKr"),
    (103350, "2F5tSQbdTfhZxvkxZqdFp7KR3FrJPKEsDLQK7KtPhNXj1EZAh4"),
    (103358, "2tCe88ur6MLQcVgwE5XxoaHiTGtSrthwKN3SdbHE4kWiQ7MSTV"),
    (103437, "21o2fVTnzzmtgXqkV1yuQeze7YEQhR5JB31jVVD9oVUnaaV8qm"),
    (103472, "2nG4exd9eUoAGzELfksmBR8XDCKhohY1uDKRFzEXJG4M8p3qA7"),
    (103478, "63YLdYXfXc5tY3mwWLaDsbXzQHYmwWVxMP7HKbRh4Du3C2iM1"),
    (103493, "soPweZ8DGaoUMjrnzjH3V2bypa7ZvvfqBan4UCsMUxMP759gw"),
    (103514, "2dNkpQF4mooveyUDfBYQTBfsGDV4wkncQPpEw4kHKfSTSTo5x"),
    (103536, "PJTkRrHvKZ1m4AQdPND1MBpUXpCrGN4DDmXmJQAiUrsxPoLQX"),
    (103545, "22ck2Z7cC38hmBfX2v3jMWxun8eD8psNaicfYeokS67DxwmPTx"),
    (103547, "pTf7gfk1ksj7bqMrLyMCij8FBKth1uRqQrtfykMFeXhx5xnrL"),
    (103554, "9oZh4qyBCcVwSGyDoUzRAuausvPJN3xH6nopKS6bwYzMfLoQ2"),
    (103555, "MjExz2z1qhwugc1tAyiGxRsCq4GvJwKfyyS29nr4tRVB8ooic"),
    (103559, "cwJusfmn98TW3DjAbfLRN9utYR24KAQ82qpAXmVSvjHyJZuM2"),
    (103561, "2YgxGHns7Z2hMMHJsPCgVXuJaL7x1b3gnHbmSCfCdyAcYGr6mx"),
    (103563, "2AXxT3PSEnaYHNtBTnYrVTf24TtKDWjky9sqoFEhydrGXE9iKH"),
    (103564, "Ry2sfjFfGEnJxRkUGFSyZNn7GR3m4aKAf1scDW2uXSNQB568Y"),
    (103569, "21Jys8UNURmtckKSV89S2hntEWymJszrLQbdLaNcbXcxDAsQSa"),
    (103570, "sg6wAwFBsPQiS5Yfyh41cVkCRQbrrXsxXmeNyQ1xkunf2sdyv"),
    (103575, "z3BgePPpCXq1mRBRvUi28rYYxnEtJizkUEHnDBrcZeVA7MFVk"),
    (103577, "uK5Ff9iBfDtREpVv9NgCQ1STD1nzLJG3yrfibHG4mGvmybw6f"),
    (103578, "Qv5v5Ru8ArfnWKB1w6s4G5EYPh7TybHJtF6UsVwAkfvZFoqmj"),
    (103582, "7KCZKBpxovtX9opb7rMRie9WmW5YbZ8A4HwBBokJ9eSHpZPqx"),
    (103587, "2AfTQ2FXNj9bkSUQnud9pFXULx6EbF7cbbw6i3ayvc2QNhgxfF"),
    (103590, "2gTygYckZgFZfN5QQWPaPBD3nabqjidV55mwy1x1Nd4JmJAwaM"),
    (103591, "2cUPPHy1hspr2nAKpQrrAEisLKkaWSS9iF2wjNFyFRs8vnSkKK"),
    (103594, "5MptSdP6dBMPSwk9GJjeVe39deZJTRh9i82cgNibjeDffrrTf"),
    (103597, "2J8z7HNv4nwh82wqRGyEHqQeuw4wJ6mCDCSvUgusBu35asnshK"),
    (103598, "2i2FP6nJyvhX9FR15qN2D9AVoK5XKgBD2i2AQ7FoSpfowxvQDX"),
    (103603, "2v3smb35s4GLACsK4Zkd2RcLBLdWA4huqrvq8Y3VP4CVe8kfTM"),
    (103604, "b7XfDDLgwB12DfL7UTWZoxwBpkLPL5mdHtXngD94Y2RoeWXSh"),
    (103607, "PgaRk1UAoUvRybhnXsrLq5t6imWhEa6ksNjbN6hWgs4qPrSzm"),
    (103612, "2oueNTj4dUE2FFtGyPpawnmCCsy6EUQeVHVLZy8NHeQmkAciP4"),
    (103614, "2YHZ1KymFjiBhpXzgt6HXJhLSt5SV9UQ4tJuUNjfN1nQQdm5zz"),
    (103617, "amgH2C1s9H3Av7vSW4y7n7TXb9tKyKHENvrDXutgNN6nsejgc"),
    (103618, "fV8k1U8oQDmfVwK66kAwN73aSsWiWhm8quNpVnKmSznBycV2W"),
    (103621, "Nzs93kFTvcXanFUp9Y8VQkKYnzmH8xykxVNFJTkdyAEeuxWbP"),
    (103623, "2rAsBj3emqQa13CV8r5fTtHogs4sXnjvbbXVzcKPi3WmzhpK9D"),
    (103624, "2JbuExUGKW5mYz5KfXATwq1ibRDimgks9wEdYGNSC6Ttey1R4U"),
    (103627, "tLLijh7oKfvWT1yk9zRv4FQvuQ5DAiuvb5kHCNN9zh4mqkFMG"),
    (103628, "dWBsRYRwFrcyi3DPdLoHsL67QkZ5h86hwtVfP94ZBaY18EkmF"),
    (103629, "XMoEsew2DhSgQaydcJFJUQAQYP8BTNTYbEJZvtbrV2QsX7iE3"),
    (103630, "2db2wMbVAoCc5EUJrsBYWvNZDekqyY8uNpaaVapdBAQZ5oRaou"),
    (103633, "2QiHZwLhQ3xLuyyfcdo5yCUfoSqWDvRZox5ECU19HiswfroCGp"),
];

/// The mainnet bonus-block skip-set as `height → block id`, decoded from the
/// CB58 ids in [`MAINNET_BONUS_BLOCKS`] (coreth
/// `readMainnetBonusBlocks`). Built once and cached.
///
/// An id that fails to decode is dropped (the table is a compile-time constant of
/// known-valid CB58 strings, so this never happens in practice) — keeping the
/// accessor infallible mirrors the atomic codec's `OnceLock` pattern.
#[must_use]
pub fn mainnet_bonus_blocks() -> &'static BTreeMap<u64, Id> {
    static BONUS: OnceLock<BTreeMap<u64, Id>> = OnceLock::new();
    BONUS.get_or_init(|| {
        let mut map = BTreeMap::new();
        for &(height, id_str) in MAINNET_BONUS_BLOCKS {
            if let Ok(id) = id_str.parse::<Id>() {
                map.insert(height, id);
            }
        }
        map
    })
}

/// Whether `(height, id)` is a mainnet bonus block whose atomic ops must be
/// **skipped** when applying to shared memory (coreth `atomic_backend.go`). The
/// height must match AND, when an id is supplied, the block id must match the
/// recorded bonus id (coreth keys the skip on height but records the id for the
/// repair audit).
#[must_use]
pub fn is_bonus_block(height: u64, id: Id) -> bool {
    mainnet_bonus_blocks().get(&height) == Some(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bonus_block_lookup() {
        let id: Id = "Njm9TcLUXRojZk8YhEM6ksvfiPdC1TME4zJvGaDXgzMCyB6oB"
            .parse()
            .expect("valid CB58 id");
        assert!(is_bonus_block(102972, id));
        assert!(!is_bonus_block(102973, id));
        // Right height, wrong id.
        assert!(!is_bonus_block(102972, Id::EMPTY));
    }

    #[test]
    fn empty_block_no_conflict() {
        let empty: BTreeSet<Id> = BTreeSet::new();
        verify_no_conflicts(&[], &empty).expect("empty block verifies");
    }
}
