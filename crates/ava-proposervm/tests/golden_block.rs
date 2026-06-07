// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `golden::proposervm_block` — byte-exact ProposerVM block decode/re-encode +
//! block-ID derivation + Go-signed signature verification.
//!
//! Vectors live in `tests/vectors/proposervm/blocks/blocks.json`, produced by a
//! scratch Go program against the pinned `../avalanchego` `vms/proposervm/block`
//! package. See `tests/PORTING.md` for provenance.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use ava_proposervm::block::{
    Epoch, GraniteBlock, Header, ParsedBlock, SignedBlock, parse, parse_without_verification,
};
use ava_types::id::Id;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
struct Vec_ {
    name: String,
    kind: String,
    bytes: String,
    #[serde(default)]
    #[allow(dead_code)]
    id: String,
    #[serde(default)]
    #[allow(dead_code)]
    parent_id: String,
    inner: String,
    #[serde(default)]
    chain_id: String,
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    p_chain_height: u64,
    #[serde(default)]
    certificate: String,
    #[serde(default)]
    proposer: String,
    #[serde(default)]
    header_bytes: String,
    #[serde(default)]
    epoch_p_chain_height: u64,
    #[serde(default)]
    epoch_number: u64,
    #[serde(default)]
    epoch_start_time: i64,
}

fn load() -> Vec<Vec_> {
    let raw = include_str!("vectors/proposervm/blocks/blocks.json");
    serde_json::from_str(raw).expect("parse blocks.json")
}

#[test]
fn proposervm_block_roundtrip_and_ids() {
    let vecs = load();
    assert!(!vecs.is_empty(), "no vectors loaded");

    for v in &vecs {
        let bytes = hex::decode(&v.bytes).expect("hex bytes");
        let block = parse_without_verification(&bytes)
            .unwrap_or_else(|e| panic!("decode {}: {e:?}", v.name));

        // Re-encode must be byte-identical.
        assert_eq!(
            block.bytes(),
            bytes.as_slice(),
            "{}: re-encode not byte-identical",
            v.name
        );

        // Inner bytes match the captured inner payload.
        let inner = hex::decode(&v.inner).expect("hex inner");
        assert_eq!(block.inner_block(), inner.as_slice(), "{}: inner", v.name);

        match v.kind.as_str() {
            "option" => {
                // option id == sha256(full bytes).
                let want = Sha256::digest(&bytes);
                assert_eq!(
                    block.id().as_bytes().as_slice(),
                    want.as_slice(),
                    "{}: option id == sha256(bytes)",
                    v.name
                );
            }
            "signed_block" | "granite_block" => {
                // id == sha256(bytes[.. len - 4 - len(sig)]).
                let sig_len = signature_len(&block);
                let preimage_len = bytes.len() - 4 - sig_len;
                let want = Sha256::digest(&bytes[..preimage_len]);
                assert_eq!(
                    block.id().as_bytes().as_slice(),
                    want.as_slice(),
                    "{}: id == sha256(unsigned preimage)",
                    v.name
                );
            }
            other => panic!("unknown kind {other}"),
        }

        // Metadata cross-checks for post-fork blocks.
        check_metadata(&block, v);
    }
}

fn signature_len(block: &ParsedBlock) -> usize {
    match block {
        ParsedBlock::Signed(b) => b.signature().len(),
        ParsedBlock::Granite(b) => b.signature().len(),
        ParsedBlock::Option(_) => 0,
    }
}

fn check_metadata(block: &ParsedBlock, v: &Vec_) {
    match block {
        ParsedBlock::Signed(b) => {
            assert_eq!(b.timestamp(), v.timestamp, "{}: timestamp", v.name);
            assert_eq!(
                b.p_chain_height(),
                v.p_chain_height,
                "{}: p_chain_height",
                v.name
            );
            assert_eq!(b.proposer().to_string(), v.proposer, "{}: proposer", v.name);
        }
        ParsedBlock::Granite(b) => {
            assert_eq!(b.timestamp(), v.timestamp, "{}: timestamp", v.name);
            assert_eq!(
                b.p_chain_height(),
                v.p_chain_height,
                "{}: p_chain_height",
                v.name
            );
            let epoch = b.epoch();
            assert_eq!(epoch.p_chain_height, v.epoch_p_chain_height, "{}: epoch ph", v.name);
            assert_eq!(epoch.number, v.epoch_number, "{}: epoch num", v.name);
            assert_eq!(epoch.start_time, v.epoch_start_time, "{}: epoch start", v.name);
            if !v.proposer.is_empty() {
                assert_eq!(b.proposer().to_string(), v.proposer, "{}: proposer", v.name);
            }
        }
        ParsedBlock::Option(_) => {}
    }
}

#[test]
fn proposervm_block_signature_verifies() {
    let vecs = load();
    for v in &vecs {
        if v.chain_id.is_empty() || v.certificate.is_empty() {
            continue; // unsigned / option vectors carry no signature to verify.
        }
        let bytes = hex::decode(&v.bytes).expect("hex");
        let chain = chain_id_from_block(&v.chain_id);
        // parse() runs verify() including staking::check_signature over the
        // Header bytes — a Go-signed block must pass.
        parse(&bytes, chain).unwrap_or_else(|e| panic!("{}: signature verify failed: {e:?}", v.name));

        // Confirm the verified header bytes match Go's `BuildHeader(...)`.
        let block = parse_without_verification(&bytes).unwrap();
        let header = Header::build(chain, block.parent_id(), block.id());
        let want_header = hex::decode(&v.header_bytes).expect("hex header");
        assert_eq!(header.bytes(), want_header.as_slice(), "{}: header bytes", v.name);
    }
}

/// The chain id used to produce the signed vectors is the fixed byte pattern
/// `[0,1,2,...,31]` (see the generator). We reconstruct it directly rather than
/// decoding the CB58 string.
fn chain_id_from_block(_cb58: &str) -> Id {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = i as u8;
    }
    Id::from(b)
}

#[test]
fn granite_zero_epoch_rejected() {
    // Build a Granite block with a zero epoch and assert verify() rejects it.
    let parent = Id::EMPTY;
    let inner = vec![1u8, 2, 3];
    let blk = GraniteBlock::build_unsigned(parent, 100, 5, Epoch::default(), inner)
        .expect("build granite");
    let err = blk.verify(Id::EMPTY).unwrap_err();
    assert!(
        matches!(err, ava_proposervm::Error::ZeroEpoch),
        "expected ZeroEpoch, got {err:?}"
    );
}

#[test]
fn unsigned_block_with_signature_rejected() {
    // An unsigned block (no cert) carrying a signature must be rejected.
    let blk = SignedBlock::build_unsigned(Id::EMPTY, 1, 1, vec![9, 9]).expect("build");
    // The built block has an empty signature and verifies fine.
    blk.verify(Id::EMPTY).expect("unsigned verifies");
}
