// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cchain/warp::from_receipts` tests (M7.38), porting Go's
//! `cchain/warp/warp_test.go::TestFromReceipts`.
//!
//! `from_receipts` scans every log in every receipt for warp-precompile-addressed
//! `SendWarpMessage` logs and unpacks each into an `UnsignedMessage`, in
//! receipt-then-log order; non-warp logs are ignored.

use assert_matches::assert_matches;
use ava_evm::precompile::warp::WARP_PRECOMPILE_ADDRESS;
use ava_evm_reth::Address;
use ava_saevm_cchain::warp::{Error, ReceiptLog, from_receipts};
use ava_types::id::Id;
use ava_warp::UnsignedMessage;
use ava_warp::payload::{AddressedCall, Hash, WarpPayload};

const NETWORK_ID: u32 = 10;

fn source_chain_id() -> Id {
    Id::from([0x5Au8; 32])
}

/// `abi.encode(bytes payload)` — a single dynamic `bytes` argument (offset word,
/// length word, padded data), matching coreth's `SendWarpMessage` log `data`.
fn abi_encode_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = [0u8; 32];
    off[31] = 0x20;
    out.extend_from_slice(&off);
    let mut len = [0u8; 32];
    len[24..].copy_from_slice(&(b.len() as u64).to_be_bytes());
    out.extend_from_slice(&len);
    out.extend_from_slice(b);
    let rem = b.len() % 32;
    if rem != 0 {
        out.extend(std::iter::repeat_n(0u8, 32usize.saturating_sub(rem)));
    }
    out
}

/// A `SendWarpMessage` warp-precompile log carrying `msg_bytes` (Go
/// `newSendWarpMessageLog`).
fn warp_log(msg_bytes: &[u8]) -> ReceiptLog {
    ReceiptLog {
        address: WARP_PRECOMPILE_ADDRESS,
        data: abi_encode_bytes(msg_bytes),
    }
}

/// A non-warp log at some other address (Go `otherLog`).
fn other_log() -> ReceiptLog {
    ReceiptLog {
        address: Address::with_last_byte(1),
        data: b"not a warp message".to_vec(),
    }
}

fn hash_message() -> UnsignedMessage {
    let payload = WarpPayload::Hash(Hash {
        hash: Id::from([0x11u8; 32]),
    })
    .marshal_payload()
    .expect("marshal_payload()");
    UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain_id(),
        payload,
    }
}

fn call_message() -> UnsignedMessage {
    let payload = WarpPayload::AddressedCall(AddressedCall {
        source_address: vec![0xABu8; 20],
        payload: b"call".to_vec(),
    })
    .marshal_payload()
    .expect("marshal_payload()");
    UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain_id(),
        payload,
    }
}

#[test]
fn from_receipts_no_receipts() {
    assert_eq!(from_receipts(&[]).expect("from_receipts()"), Vec::new());
}

#[test]
fn from_receipts_no_logs() {
    let receipts = vec![Vec::new()];
    assert_eq!(
        from_receipts(&receipts).expect("from_receipts()"),
        Vec::new()
    );
}

#[test]
fn from_receipts_ignores_other_addresses() {
    let receipts = vec![vec![other_log()]];
    assert_eq!(
        from_receipts(&receipts).expect("from_receipts()"),
        Vec::new()
    );
}

#[test]
fn from_receipts_single_message() {
    let hash = hash_message();
    let receipts = vec![vec![warp_log(&hash.marshal().expect("marshal"))]];
    assert_eq!(
        from_receipts(&receipts).expect("from_receipts()"),
        vec![hash]
    );
}

#[test]
fn from_receipts_multiple_messages_in_order() {
    let hash = hash_message();
    let call = call_message();
    let warp_hash = warp_log(&hash.marshal().expect("marshal"));
    let warp_call = warp_log(&call.marshal().expect("marshal"));
    let receipts = vec![
        vec![warp_call.clone(), other_log(), warp_hash.clone()],
        vec![other_log(), warp_call.clone()],
    ];
    assert_eq!(
        from_receipts(&receipts).expect("from_receipts()"),
        vec![call.clone(), hash, call]
    );
}

#[test]
fn from_receipts_invalid_log_data() {
    // An empty inner-bytes payload is not a valid unsigned-message format.
    let receipts = vec![vec![warp_log(&[])]];
    assert_matches!(from_receipts(&receipts), Err(Error::Warp(_)));
}
