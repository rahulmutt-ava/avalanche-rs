// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `initialize_chain_state` genesis seeding + idempotency + byte-detail
//! persistence (M5.11, specs 09 §1/§5/§5.3, 07).
//!
//! Mirrors `vms/avm/state/state.go`'s `InitializeChainState`: on a fresh state
//! (no stored `lastAccepted`) it seeds a genesis [`StandardBlock`](
//! ava_avm::block::StandardBlock) with `parent = stop_vertex_id`, `height = 0`,
//! `time = genesis_ts`, no txs; persists it (block store + height index + last
//! accepted + timestamp + initialized flag) and commits. A second call is a
//! no-op (idempotent). Asserts the on-disk byte details: the `height → blockID`
//! index key is 8-byte big-endian (`database.PackUInt64`) and the timestamp is
//! the Unix-second value (`database.PutTimestamp`).

#![allow(unused_crate_dependencies)]
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arithmetic_side_effects)]

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ava_avm::block::{Block, StandardBlock};
use ava_avm::state::{ReadOnlyChain, State};
use ava_avm::txs::codec::codec;
use ava_database::KeyValueReader;
use ava_database::MemDb;
use ava_database::prefixdb::make_prefix;
use ava_types::id::Id;

/// A 32-byte id built from a single seed byte.
fn id(seed: u8) -> Id {
    Id::from([seed; 32])
}

/// The genesis timestamp used across the tests: 1 234 567 890 Unix seconds.
fn genesis_ts() -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_234_567_890)
}

/// Builds the expected genesis block to compare ids/bytes against.
fn expected_genesis(stop_vertex: Id, ts_secs: u64) -> Block {
    let c = codec().expect("codec");
    StandardBlock::new_block(&c, stop_vertex, 0, ts_secs, Vec::new()).expect("genesis block")
}

#[test]
fn seeds_genesis_on_fresh_state() {
    let c = codec().expect("codec");
    let mut s = State::new(Arc::new(MemDb::new())).expect("state");
    assert!(!s.is_initialized().expect("is_initialized"));

    let stop_vertex = id(0xAB);
    s.initialize_chain_state(stop_vertex, genesis_ts(), &c)
        .expect("initialize");

    let expected = expected_genesis(stop_vertex, 1_234_567_890);

    // Singletons set.
    assert!(s.is_initialized().expect("is_initialized"));
    assert_eq!(s.get_last_accepted(), expected.id());
    assert_eq!(s.get_timestamp(), genesis_ts());

    // Genesis block persisted, addressable by id and by height 0.
    let stored = s.get_block(expected.id()).expect("get_block");
    assert_eq!(stored, expected.bytes());
    assert_eq!(s.get_block_id_at_height(0), Some(expected.id()));

    // Re-parse the stored block: parent = stop_vertex, height 0, time, no txs.
    let parsed = Block::parse(&c, &stored).expect("parse");
    assert_eq!(parsed.parent_id(), stop_vertex);
    assert_eq!(parsed.height(), 0);
    assert_eq!(parsed.timestamp(), 1_234_567_890);
    assert!(parsed.txs().is_empty());
}

#[test]
fn idempotent_second_call_does_not_reseed() {
    let c = codec().expect("codec");
    let mut s = State::new(Arc::new(MemDb::new())).expect("state");

    let stop_vertex = id(0x11);
    s.initialize_chain_state(stop_vertex, genesis_ts(), &c)
        .expect("first init");
    let first_last = s.get_last_accepted();
    let first_ts = s.get_timestamp();

    // A second call with a *different* stop-vertex/timestamp must NOT re-seed:
    // the previously persisted last-accepted and timestamp are loaded back.
    s.initialize_chain_state(id(0x22), genesis_ts() + Duration::from_secs(999), &c)
        .expect("second init");

    assert_eq!(s.get_last_accepted(), first_last);
    assert_eq!(s.get_timestamp(), first_ts);
    // No second genesis block seeded.
    assert!(s.get_block_id_at_height(1).is_none());
}

#[test]
fn persisted_state_survives_reopen() {
    let base = Arc::new(MemDb::new());
    let c = codec().expect("codec");
    let stop_vertex = id(0x77);

    let expected = expected_genesis(stop_vertex, 1_234_567_890);
    {
        let mut s = State::new(Arc::clone(&base)).expect("state");
        s.initialize_chain_state(stop_vertex, genesis_ts(), &c)
            .expect("init");
    }

    // Reopen over the same committed base; load singletons; init is now a no-op.
    let mut s = State::new(Arc::clone(&base)).expect("reopen");
    s.initialize_chain_state(id(0x00), genesis_ts() + Duration::from_secs(5), &c)
        .expect("reopen init");
    assert_eq!(s.get_last_accepted(), expected.id());
    assert_eq!(s.get_timestamp(), genesis_ts());
    assert_eq!(s.get_block_id_at_height(0), Some(expected.id()));
}

/// Byte-detail assertions over the raw committed base DB: the height index key
/// is 8-byte big-endian, the timestamp singleton is the Unix-second value.
#[test]
fn persistence_byte_details() {
    let base = Arc::new(MemDb::new());
    let c = codec().expect("codec");
    let stop_vertex = id(0x55);
    let expected = expected_genesis(stop_vertex, 1_234_567_890);

    {
        let mut s = State::new(Arc::clone(&base)).expect("state");
        s.initialize_chain_state(stop_vertex, genesis_ts(), &c)
            .expect("init");
    }

    // On-disk keys are `SHA256(prefix) ‖ key` (PrefixDb namespacing).
    let prefixed = |prefix: &[u8], key: &[u8]| -> Vec<u8> {
        let mut k = make_prefix(prefix);
        k.extend_from_slice(key);
        k
    };

    // height index: "blockID" prefix + 8-byte big-endian height (0).
    let stored_id = base
        .get(&prefixed(b"blockID", &0u64.to_be_bytes()))
        .expect("height index present");
    assert_eq!(stored_id, expected.id().as_bytes());

    // timestamp singleton: "singleton" prefix + 0x01 -> 8-byte big-endian secs.
    let stored_ts = base
        .get(&prefixed(b"singleton", &[0x01]))
        .expect("timestamp present");
    assert_eq!(stored_ts, 1_234_567_890u64.to_be_bytes());

    // last-accepted singleton: "singleton" prefix + 0x02 -> 32-byte block id.
    let stored_la = base
        .get(&prefixed(b"singleton", &[0x02]))
        .expect("last accepted present");
    assert_eq!(stored_la, expected.id().as_bytes());

    // initialized singleton: "singleton" prefix + 0x00 -> empty value present.
    assert!(base.get(&prefixed(b"singleton", &[0x00])).is_ok());
}
