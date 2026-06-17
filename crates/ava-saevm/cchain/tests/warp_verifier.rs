// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cchain/warp` ACP-118 sign-decision tests (M7.38), porting Go's
//! `cchain/warp/verifier_test.go::TestVerifier`.
//!
//! Every refusal carries one of the four `iota+1` codes, asserted here to be
//! exactly `1`/`2`/`3`/`4` for p2p `AppError` parity.

use std::collections::BTreeSet;
use std::sync::Arc;

use ava_database::MemDb;
use ava_database::traits::Database;
use ava_saevm_cchain::warp::storage::Storage;
use ava_saevm_cchain::warp::verifier::{AppErrorCode, Backend, Verifier};
use ava_types::id::Id;
use ava_warp::UnsignedMessage;
use ava_warp::payload::{AddressedCall, Hash, WarpPayload};

const NETWORK_ID: u32 = 10;

fn source_chain_id() -> Id {
    Id::from([0x5Au8; 32])
}

/// A backend that reports a fixed set of block ids accepted (Go `backend`).
struct AcceptedSet(BTreeSet<Id>);

impl Backend for AcceptedSet {
    fn is_accepted(&self, block_id: Id) -> bool {
        self.0.contains(&block_id)
    }
}

/// An addressed-call message (parses, but is NOT a block hash).
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

/// A block-hash attestation message + the attested block id (Go `newHash`).
fn new_hash() -> (UnsignedMessage, Id) {
    let hash = Id::from([0x77u8; 32]);
    let payload = WarpPayload::Hash(Hash { hash })
        .marshal_payload()
        .expect("marshal_payload()");
    (
        UnsignedMessage {
            network_id: NETWORK_ID,
            source_chain_id: source_chain_id(),
            payload,
        },
        hash,
    )
}

/// A message with a payload that does not parse as any warp payload (Go's
/// `warp.NewUnsignedMessage(..., nil)`).
fn invalid_payload_msg() -> UnsignedMessage {
    UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain_id(),
        payload: Vec::new(),
    }
}

/// `known_message`: the message is in storage, so the node signs it.
#[test]
fn verifier_known_message() {
    let msg = new_addressed_call();
    let db = Arc::new(MemDb::new());
    let storage = Storage::new(db, std::slice::from_ref(&msg)).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::new()), &storage);
    assert_eq!(v.verify(&msg), Ok(()));
}

/// `storage_error`: a closed DB makes `Get` fail with a non-`NotFound` error,
/// surfaced as `StorageErrCode` (1).
#[test]
fn verifier_storage_error() {
    let msg = new_addressed_call();
    let db = Arc::new(MemDb::new());
    db.close().expect("MemDb::close()");
    let storage = Storage::new(db, &[]).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::new()), &storage);

    let err = v
        .verify(&msg)
        .expect_err("Verifier::verify() should refuse");
    assert_eq!(err.code, AppErrorCode::Storage);
    assert_eq!(err.code.code(), 1);
}

/// `invalid_payload`: an unparseable payload → `ParseErrCode` (2).
#[test]
fn verifier_invalid_payload() {
    let msg = invalid_payload_msg();
    let db = Arc::new(MemDb::new());
    let storage = Storage::new(db, &[]).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::new()), &storage);

    let err = v
        .verify(&msg)
        .expect_err("Verifier::verify() should refuse");
    assert_eq!(err.code, AppErrorCode::Parse);
    assert_eq!(err.code.code(), 2);
}

/// `unknown_message`: an addressed-call (parses, not a block hash) not in storage
/// → `UnknownMessageErrCode` (3).
#[test]
fn verifier_unknown_message() {
    let msg = new_addressed_call();
    let db = Arc::new(MemDb::new());
    let storage = Storage::new(db, &[]).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::new()), &storage);

    let err = v
        .verify(&msg)
        .expect_err("Verifier::verify() should refuse");
    assert_eq!(err.code, AppErrorCode::Unknown);
    assert_eq!(err.code.code(), 3);
}

/// `accepted_block`: a block-hash attestation of an accepted block → signed.
#[test]
fn verifier_accepted_block() {
    let (msg, hash) = new_hash();
    let db = Arc::new(MemDb::new());
    let storage = Storage::new(db, &[]).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::from([hash])), &storage);
    assert_eq!(v.verify(&msg), Ok(()));
}

/// `unaccepted_block`: a block-hash attestation of an unaccepted block →
/// `NotAcceptedErrCode` (4).
#[test]
fn verifier_unaccepted_block() {
    let (msg, _hash) = new_hash();
    let db = Arc::new(MemDb::new());
    let storage = Storage::new(db, &[]).expect("Storage::new()");
    let v = Verifier::new(AcceptedSet(BTreeSet::new()), &storage);

    let err = v
        .verify(&msg)
        .expect_err("Verifier::verify() should refuse");
    assert_eq!(err.code, AppErrorCode::NotAccepted);
    assert_eq!(err.code.code(), 4);
}
