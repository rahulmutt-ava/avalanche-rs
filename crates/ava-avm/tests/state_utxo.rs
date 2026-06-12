// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! UTXO state stores + Diff layer (M5.10, specs 09 §5/§5.1/§5.2).
//!
//! Exercises the persisted [`State`](ava_avm::state::State) over an in-memory
//! base DB (add UTXO → commit → reopen → identical bytes; add tx → get tx parses
//! via the genesis codec), the [`Diff`](ava_avm::state::Diff) overlay (delete +
//! apply removes; abort discards), and the determinism contract (00 §6.1): a
//! `Diff` flush emits keys in sorted (`BTreeMap`) order regardless of insertion
//! order.

#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects)]

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_avm::state::{Chain, Diff, ReadOnlyChain, State};
// Txs are read back via the genesis codec (specs 09 §5.3) — the real 21-entry
// `txs::codec::GenesisCodec()` singleton from M5.5. The storage layer itself is
// codec-agnostic and only round-trips opaque tx bytes.
use ava_avm::txs::codec::GenesisCodec;
use ava_avm::txs::{BaseTx, Tx, UnsignedTx};
use ava_database::MemDb;
use ava_database::error::Error as DbError;
use ava_types::id::Id;
use ava_vm::components::avax::UtxoId;
use proptest::prelude::*;

/// A 32-byte id built from a single seed byte (distinct ids for tests).
fn id(seed: u8) -> Id {
    Id::from([seed; 32])
}

/// Distinct UTXO bytes for a given id.
fn utxo_bytes(seed: u8) -> Vec<u8> {
    vec![seed; 16]
}

#[test]
fn add_commit_reopen_get_utxo_roundtrips() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let utxo_id = UtxoId::new(id(1), 0).input_id();
    let bytes = utxo_bytes(7);
    state.add_utxo(utxo_id, bytes.clone());

    // Before commit the base DB is untouched; the modified UTXO is served from
    // the in-memory overlay.
    assert_eq!(state.get_utxo(utxo_id).expect("get utxo"), bytes);

    state.commit().expect("commit");

    // Reopen a fresh State over the same base DB and read the persisted bytes.
    let reopened = State::new(Arc::clone(&base)).expect("reopen");
    assert_eq!(reopened.get_utxo(utxo_id).expect("get utxo"), bytes);
}

#[test]
fn delete_utxo_commit_reopen_is_not_found() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let utxo_id = UtxoId::new(id(2), 1).input_id();
    state.add_utxo(utxo_id, utxo_bytes(3));
    state.commit().expect("commit");

    let mut state = State::new(Arc::clone(&base)).expect("reopen");
    state.delete_utxo(utxo_id);
    state.commit().expect("commit");

    let reopened = State::new(Arc::clone(&base)).expect("reopen");
    assert!(matches!(
        reopened.get_utxo(utxo_id),
        Err(ava_avm::Error::Database(DbError::NotFound))
    ));
}

#[test]
fn add_tx_then_get_tx_parses_via_genesis_codec() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let c = GenesisCodec();
    let mut tx = Tx::new(UnsignedTx::Base(BaseTx::default()));
    tx.initialize(c).expect("initialize");
    let tx_id = tx.id();
    let want_bytes = tx.bytes().to_vec();

    state.add_tx(tx_id, want_bytes.clone());
    state.commit().expect("commit");

    let reopened = State::new(Arc::clone(&base)).expect("reopen");
    let got = reopened.get_tx(tx_id).expect("get tx");
    // The stored bytes round-trip through the genesis codec into the same Tx.
    let parsed = Tx::parse(c, &got).expect("parse");
    assert_eq!(parsed.id(), tx_id);
    assert_eq!(parsed.unsigned, tx.unsigned);
    assert_eq!(got, want_bytes);
}

#[test]
fn block_store_roundtrips_bytes_and_height_index() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let blk_id = id(9);
    let blk_bytes = vec![0xab; 24];
    state.add_block(blk_id, 5, blk_bytes.clone());
    state.commit().expect("commit");

    let reopened = State::new(Arc::clone(&base)).expect("reopen");
    assert_eq!(reopened.get_block(blk_id).expect("get block"), blk_bytes);
    assert_eq!(
        reopened.get_block_id_at_height(5).expect("height index"),
        blk_id
    );
}

#[test]
fn singletons_last_accepted_and_timestamp_persist() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let la = id(42);
    let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    state.set_last_accepted(la);
    state.set_timestamp(ts);
    state.commit().expect("commit");

    let mut reopened = State::new(Arc::clone(&base)).expect("reopen");
    reopened.load().expect("load singletons");
    assert_eq!(reopened.get_last_accepted(), la);
    assert_eq!(reopened.get_timestamp(), ts);
}

#[test]
fn diff_delete_then_apply_removes_utxo() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let utxo_id = UtxoId::new(id(4), 0).input_id();
    state.add_utxo(utxo_id, utxo_bytes(5));

    let parent: Arc<dyn Chain> = state.snapshot();
    let mut diff = Diff::new_on(parent).expect("diff");

    // The diff sees the parent's UTXO until it deletes it.
    assert_eq!(diff.get_utxo(utxo_id).expect("get"), utxo_bytes(5));
    diff.delete_utxo(utxo_id);
    assert!(matches!(
        diff.get_utxo(utxo_id),
        Err(ava_avm::Error::Database(DbError::NotFound))
    ));

    // Applying the diff onto the base state removes the UTXO there too.
    diff.apply(&mut state);
    assert!(matches!(
        state.get_utxo(utxo_id),
        Err(ava_avm::Error::Database(DbError::NotFound))
    ));
}

#[test]
fn diff_abort_discards_changes() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");

    let utxo_id = UtxoId::new(id(6), 2).input_id();
    state.add_utxo(utxo_id, utxo_bytes(8));

    let parent: Arc<dyn Chain> = state.snapshot();
    let mut diff = Diff::new_on(parent).expect("diff");
    diff.delete_utxo(utxo_id);

    // Aborting (dropping the diff without apply) leaves the base unchanged.
    drop(diff);
    assert_eq!(state.get_utxo(utxo_id).expect("get"), utxo_bytes(8));
}

proptest! {
    /// A `Diff` flush emits modified-UTXO keys in sorted order regardless of the
    /// order they were inserted (00 §6.1 — `BTreeMap` on flush).
    #[test]
    fn diff_flush_is_sorted(mut seeds in proptest::collection::vec(any::<u8>(), 1..32)) {
        seeds.sort_unstable();
        seeds.dedup();

        let base = Arc::new(MemDb::new());
        let state = State::new(Arc::clone(&base)).expect("state");
        let parent: Arc<dyn Chain> = state.snapshot();
        let mut diff = Diff::new_on(parent).expect("diff");

        // Insert UTXOs in a shuffled (reverse) order relative to their ids.
        for &s in seeds.iter().rev() {
            diff.add_utxo(id(s), utxo_bytes(s));
        }

        // The flush key order must be ascending by id, independent of insert order.
        let flushed = diff.flush_utxo_ids();
        let mut sorted = flushed.clone();
        sorted.sort();
        prop_assert_eq!(flushed, sorted);
    }
}

// ---------------------------------------------------------------------------
// M8.23b — the address → UTXO index (Go `avax.utxoState` index semantics)
// ---------------------------------------------------------------------------

/// A 20-byte address from a single seed byte.
fn short(seed: u8) -> ava_types::short_id::ShortId {
    ava_types::short_id::ShortId::from_slice(&[seed; 20]).expect("short id")
}

/// Canonical `avax.UTXO` bytes carrying a secp `TransferOutput` owned by `addrs`.
fn owned_utxo(tx_seed: u8, out_index: u32, amt: u64, addrs: &[u8]) -> (Id, Vec<u8>) {
    use ava_avm::txs::components::Output;
    use ava_avm::txs::executor::semantic::Utxo as AvmUtxo;
    use ava_secp256k1fx::{OutputOwners, TransferOutput};

    let utxo = AvmUtxo {
        tx_id: id(tx_seed),
        output_index: out_index,
        asset_id: id(0xAA),
        out: Output::SecpTransfer(TransferOutput::new(
            amt,
            OutputOwners::new(0, 1, addrs.iter().map(|&a| short(a)).collect()),
        )),
    };
    (utxo.input_id(), utxo.marshal().expect("marshal utxo"))
}

#[test]
fn utxo_ids_indexes_owning_addresses_on_add() {
    let mut state = State::new(Arc::new(MemDb::new())).expect("state");

    // Two UTXOs for addr 1, one of them shared with addr 2.
    let (id_a, bytes_a) = owned_utxo(1, 0, 100, &[1]);
    let (id_b, bytes_b) = owned_utxo(2, 0, 200, &[1, 2]);
    state.add_utxo(id_a, bytes_a);
    state.add_utxo(id_b, bytes_b);

    let mut expected_1 = vec![id_a, id_b];
    expected_1.sort();
    assert_eq!(
        state
            .utxo_ids(&short(1), Id::EMPTY, 1024)
            .expect("utxo_ids addr1"),
        expected_1,
        "addr 1 sees both UTXOs in sorted order"
    );
    assert_eq!(
        state
            .utxo_ids(&short(2), Id::EMPTY, 1024)
            .expect("utxo_ids addr2"),
        vec![id_b],
        "addr 2 sees only the shared UTXO"
    );
    assert_eq!(
        state
            .utxo_ids(&short(9), Id::EMPTY, 1024)
            .expect("utxo_ids addr9"),
        Vec::<Id>::new(),
        "an unrelated address sees nothing"
    );
}

#[test]
fn utxo_ids_delete_removes_index_rows() {
    let mut state = State::new(Arc::new(MemDb::new())).expect("state");
    let (id_a, bytes_a) = owned_utxo(1, 0, 100, &[1]);
    let (id_b, bytes_b) = owned_utxo(2, 0, 200, &[1]);
    state.add_utxo(id_a, bytes_a);
    state.add_utxo(id_b, bytes_b);

    state.delete_utxo(id_a);
    assert_eq!(
        state
            .utxo_ids(&short(1), Id::EMPTY, 1024)
            .expect("utxo_ids"),
        vec![id_b],
        "deleting a UTXO removes its index row"
    );
    // Deleting an absent UTXO is a no-op (Go DeleteUTXO returns nil early).
    state.delete_utxo(id_a);
    assert_eq!(
        state
            .utxo_ids(&short(1), Id::EMPTY, 1024)
            .expect("utxo_ids"),
        vec![id_b]
    );
}

#[test]
fn utxo_ids_pagination_previous_and_limit() {
    let mut state = State::new(Arc::new(MemDb::new())).expect("state");
    let mut ids = Vec::new();
    for seed in 1..=5u8 {
        let (uid, bytes) = owned_utxo(seed, 0, 10, &[7]);
        state.add_utxo(uid, bytes);
        ids.push(uid);
    }
    ids.sort();

    // limit truncates.
    let first_two = state.utxo_ids(&short(7), Id::EMPTY, 2).expect("utxo_ids");
    assert_eq!(first_two, ids[..2].to_vec(), "limit returns the first page");

    // `previous` resumes strictly after the cursor (Go skips utxoID == start).
    let rest = state
        .utxo_ids(&short(7), first_two[1], 1024)
        .expect("utxo_ids resume");
    assert_eq!(
        rest,
        ids[2..].to_vec(),
        "previous cursor resumes after itself"
    );
}

#[test]
fn utxo_ids_survive_commit_and_reopen() {
    let base = Arc::new(MemDb::new());
    let mut state = State::new(Arc::clone(&base)).expect("state");
    let (id_a, bytes_a) = owned_utxo(1, 0, 100, &[3]);
    state.add_utxo(id_a, bytes_a);
    state.commit().expect("commit");

    let reopened = State::new(base).expect("reopen");
    assert_eq!(
        reopened
            .utxo_ids(&short(3), Id::EMPTY, 1024)
            .expect("utxo_ids"),
        vec![id_a],
        "the index persists across commit + reopen"
    );
}

#[test]
fn utxo_ids_opaque_bytes_are_not_indexed() {
    // Bytes that do not parse as an `avax.UTXO` (Go: a non-`Addressable`
    // output) are stored but never indexed.
    let mut state = State::new(Arc::new(MemDb::new())).expect("state");
    let uid = UtxoId::new(id(1), 0).input_id();
    state.add_utxo(uid, utxo_bytes(9));
    assert_eq!(state.get_utxo(uid).expect("get"), utxo_bytes(9));
    assert_eq!(
        state
            .utxo_ids(&short(1), Id::EMPTY, 1024)
            .expect("utxo_ids"),
        Vec::<Id>::new()
    );
}

#[test]
fn utxo_ids_unavailable_on_diff() {
    // A `Diff` carries no address index (Go: only `avax.utxoState` implements
    // `UTXOIDs`); the default trait impl errors.
    let state = State::new(Arc::new(MemDb::new())).expect("state");
    let parent: Arc<dyn Chain> = state.snapshot();
    let diff = Diff::new_on(parent).expect("diff");
    assert!(diff.utxo_ids(&short(1), Id::EMPTY, 1024).is_err());
}
