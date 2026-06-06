// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M1.19 state-sync exit tests (specs 04 §3.7, 19 §4/§8, 15 §3.10).
//!
//! - `golden::sync_proof_wire` — `ProofRequest`/`ProofResponse` proto frames
//!   (incl. bare `MaybeBytes`) round-trip byte-exact against **real Go-extracted**
//!   vectors under `tests/vectors/sync/wire/proof_frames.json` (rev fb174e8925).
//! - `prop::sync_proof_roundtrip` — a server's `range_proof`/`change_proof`
//!   verify against the client and the committed root equals the byte-exact
//!   target root, including an `UpdateSyncTarget` mid-sync that advances the root
//!   so the final root is the *new* target.
//!
//! Gated on the `sync` feature; the green gate / nextest run `--all-features`.

#![cfg(feature = "sync")]

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use proptest::collection::btree_map;
use proptest::prelude::*;

use ava_merkledb::key::BranchFactor;
use ava_merkledb::sync::proto::{self, ProofRequest, ProofResponse};
use ava_merkledb::sync::syncer::{LocalClient, SyncClient, Syncer, SyncerConfig};
use ava_merkledb::sync::{ProofServer, SyncDb, SyncableTrie};
use ava_types::id::Id;

// ---------------------------------------------------------------------------
// golden::sync_proof_wire
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct WireVector {
    name: String,
    hex: String,
}

fn load_wire_vectors() -> Vec<WireVector> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/sync/wire/proof_frames.json"
    ))
    .expect("read sync wire vectors");
    serde_json::from_str(&raw).expect("parse sync wire vectors")
}

/// Re-encodes each Go-extracted frame from its decoded form and asserts the
/// bytes round-trip exactly (prost decode -> prost encode == Go deterministic
/// marshal). proto3 + prost both emit fields in tag order with no map here, so
/// the re-encoded bytes equal Go's deterministic output.
#[test]
fn sync_proof_wire() {
    let vectors = load_wire_vectors();
    let mut by_name: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for v in &vectors {
        by_name.insert(v.name.clone(), hex::decode(&v.hex).expect("hex"));
    }

    // ProofRequest frames: decode then re-encode -> identical bytes.
    for name in [
        "range_proof_request_bounded",
        "range_proof_request_unbounded_start",
        "range_proof_request_empty_start_value",
        "change_proof_request_bounded",
        "change_proof_request_unbounded",
    ] {
        let want = &by_name[name];
        let req: ProofRequest = proto::decode(want).expect("decode ProofRequest");
        let got = proto::encode(&req);
        assert_eq!(&got, want, "ProofRequest frame {name} not byte-exact");
    }

    // ProofResponse frames.
    for name in ["proof_response_range", "proof_response_change"] {
        let want = &by_name[name];
        let resp: ProofResponse = proto::decode(want).expect("decode ProofResponse");
        let got = proto::encode(&resp);
        assert_eq!(&got, want, "ProofResponse frame {name} not byte-exact");
    }

    // Verify our request *constructors* produce the exact Go-extracted bytes for
    // the bounded range/change requests (this exercises maybe_to_proto + the
    // field-tag layout, not just the decode/encode identity).
    let root_hash = {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = 0xA0u8.wrapping_add(u8::try_from(i).unwrap());
        }
        Id::from(b)
    };
    let range_req = proto::range_proof_request(root_hash, Some(b"key0"), Some(b"key9"), 2048, 1024);
    assert_eq!(
        proto::encode(&range_req),
        by_name["range_proof_request_bounded"],
        "constructed range request not byte-exact with Go"
    );

    let start_root = {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = u8::try_from(i + 1).unwrap();
        }
        Id::from(b)
    };
    let end_root = {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = 0x80u8.wrapping_add(u8::try_from(i).unwrap());
        }
        Id::from(b)
    };
    let change_req =
        proto::change_proof_request(start_root, end_root, Some(b"aaa"), Some(b"bbb"), 512, 4096);
    assert_eq!(
        proto::encode(&change_req),
        by_name["change_proof_request_bounded"],
        "constructed change request not byte-exact with Go"
    );

    // Bare MaybeBytes framing: present(empty) marshals to empty bytes (the only
    // field is empty bytes which proto3 omits) — matches Go.
    let present = proto::MaybeBytes {
        value: Bytes::from_static(b"hello"),
    };
    assert_eq!(
        proto::encode(&present),
        by_name["maybe_bytes_present"],
        "MaybeBytes(present) framing"
    );
    let present_empty = proto::MaybeBytes {
        value: Bytes::new(),
    };
    assert_eq!(
        proto::encode(&present_empty),
        by_name["maybe_bytes_present_empty"],
        "MaybeBytes(present empty) framing"
    );
}

// ---------------------------------------------------------------------------
// prop::sync_proof_roundtrip
// ---------------------------------------------------------------------------

fn key_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(0u8..6, 1..3)
}

fn value_strategy() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..40)
}

fn kvs_strategy() -> impl Strategy<Value = BTreeMap<Vec<u8>, Vec<u8>>> {
    btree_map(key_strategy(), value_strategy(), 1..12)
}

fn pairs(m: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<(Vec<u8>, Vec<u8>)> {
    m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Drives a full sync of an empty client trie toward a server holding `server`,
/// then (if `advance` is non-empty) advances the server to a second state and
/// re-syncs via `update_sync_target`. Asserts the final client root equals the
/// (final) server target root, byte-for-byte.
fn run_sync(server_kvs: &BTreeMap<Vec<u8>, Vec<u8>>, advance: &BTreeMap<Vec<u8>, Vec<u8>>) {
    let bf = BranchFactor::Sixteen;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(async move {
        // Server holds the target state.
        let server_pairs = pairs(server_kvs);
        let server_refs: Vec<(&[u8], &[u8])> = server_pairs
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        let server_db = Arc::new(SyncableTrie::from_kvs(bf, &server_refs));
        let target = server_db.merkle_root().expect("server root");

        // Client starts empty.
        let client_db = Arc::new(SyncableTrie::new(bf));
        let proof_server = Arc::new(ProofServer::new(Arc::clone(&server_db)));
        let client: Arc<dyn SyncClient> = Arc::new(LocalClient::new(Arc::clone(&proof_server)));

        let syncer = Syncer::new(
            Arc::clone(&client_db),
            client,
            target,
            SyncerConfig::default(),
        );
        syncer.sync().await.expect("initial sync");

        let got = client_db.merkle_root().expect("client root");
        assert_eq!(
            got, target,
            "client root != server target after initial sync"
        );

        if !advance.is_empty() {
            // Advance the server to a new state (a superset/overwrite).
            let mut new_state = server_kvs.clone();
            for (k, v) in advance {
                new_state.insert(k.clone(), v.clone());
            }
            let np = pairs(&new_state);
            let nrefs: Vec<(&[u8], &[u8])> = np
                .iter()
                .map(|(k, v)| (k.as_slice(), v.as_slice()))
                .collect();
            let new_server = Arc::new(SyncableTrie::from_kvs(bf, &nrefs));
            let new_target = new_server.merkle_root().expect("new server root");

            // Rewire the proof server to the new state and advance the target.
            let new_proof_server = Arc::new(ProofServer::new(Arc::clone(&new_server)));
            let new_client: Arc<dyn SyncClient> =
                Arc::new(LocalClient::new(Arc::clone(&new_proof_server)));
            let syncer2 = Syncer::new(
                Arc::clone(&client_db),
                new_client,
                target,
                SyncerConfig::default(),
            );
            // Mid-sync target advance (spec 19 §8): set the new target before
            // running, exercising update_sync_target's re-queue path.
            syncer2.update_sync_target(new_target);
            syncer2.sync().await.expect("re-sync to advanced target");

            let got2 = client_db.merkle_root().expect("client root 2");
            assert_eq!(got2, new_target, "client root != new target after advance");
        }
    });
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn sync_proof_roundtrip(
        server in kvs_strategy(),
        advance in kvs_strategy(),
    ) {
        run_sync(&server, &advance);
    }
}
