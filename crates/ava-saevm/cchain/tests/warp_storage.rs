// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cchain/warp` message store tests (M7.38), porting Go's
//! `cchain/warp/storage_test.go::TestStorage`.
//!
//! Each case is run twice — once against the live store (cache populated) and
//! once against a freshly re-created store over the same DB (cache cold) — to
//! confirm the DB layer round-trips independently of the cache.

use std::sync::Arc;

use assert_matches::assert_matches;
use ava_database::{Error as DbError, MemDb};
use ava_saevm_cchain::warp::Error;
use ava_saevm_cchain::warp::storage::Storage;
use ava_types::id::Id;
use ava_warp::UnsignedMessage;
use ava_warp::payload::{AddressedCall, WarpPayload};

const NETWORK_ID: u32 = 10;

/// The source-chain id the test messages claim to originate from
/// (Go `snowtest.XChainID`).
fn source_chain_id() -> Id {
    Id::from([0x5Au8; 32])
}

/// A structurally-valid addressed-call warp message (Go `newAddressedCall`).
fn new_addressed_call() -> UnsignedMessage {
    let call = AddressedCall {
        source_address: vec![0xABu8; 20],
        payload: b"test".to_vec(),
    };
    let payload = WarpPayload::AddressedCall(call)
        .marshal_payload()
        .expect("marshal_payload()");
    UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain_id(),
        payload,
    }
}

#[test]
fn storage_add_get() {
    let msg = new_addressed_call();
    let id = msg.id().expect("UnsignedMessage::id()");
    let db = Arc::new(MemDb::new());

    let s = Storage::new(Arc::clone(&db), &[]).expect("Storage::new()");
    s.add(std::slice::from_ref(&msg)).expect("Storage::add()");

    // after_add: served from the cache.
    assert_eq!(s.get(id).expect("Storage::get() after_add"), msg);

    // fresh: a re-created store over the same DB reads it back from the DB.
    let s2 = Storage::new(db, &[]).expect("Storage::new() fresh");
    assert_eq!(s2.get(id).expect("Storage::get() fresh"), msg);
}

#[test]
fn storage_get_override() {
    let msg = new_addressed_call();
    let id = msg.id().expect("UnsignedMessage::id()");
    let db = Arc::new(MemDb::new());

    // The override is held in memory; no Add is performed.
    let s = Storage::new(Arc::clone(&db), std::slice::from_ref(&msg)).expect("Storage::new()");
    assert_eq!(s.get(id).expect("Storage::get() override"), msg);

    // A freshly re-created store still serves the override (it is in-memory).
    let s2 = Storage::new(db, std::slice::from_ref(&msg)).expect("Storage::new() fresh");
    assert_eq!(s2.get(id).expect("Storage::get() override fresh"), msg);
}

#[test]
fn storage_get_unknown() {
    let msg = new_addressed_call();
    let id = msg.id().expect("UnsignedMessage::id()");
    let db = Arc::new(MemDb::new());

    let s = Storage::new(db, &[]).expect("Storage::new()");
    assert_matches!(s.get(id), Err(Error::Db(DbError::NotFound)));
}
