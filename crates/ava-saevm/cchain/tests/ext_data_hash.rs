// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain `ParseBlock` extData-hash verification (specs/11 §8 + `10` §9
//! upstream-delta; Go `cchain/vm.go::ParseBlock` #5447 +
//! `customtypes/block_ext.go::CalcExtDataHash`).
//!
//! The SAE block ID is the keccak256 of its (RLP) header, which commits the
//! `ExtDataHash`. A C-Chain block carries its atomic txs as `extData` — a
//! trailing RLP byte-string appended after the bare SAE eth block (approach (B),
//! M7.37: the SAE core stays a stock alloy block, the C-Chain layer owns the
//! extData carrier). Because the commitment is in the header (and therefore the
//! block ID), a tampered `extData` body keeps the same ID, so the base SAE
//! `ParseBlock` — unaware of the C-Chain extData concept — would accept it. The
//! C-Chain `Vm::parse_block` override is the boundary that recomputes
//! `CalcExtDataHash(extData)` and rejects a mismatch.
//!
//! Mirrors `vms/saevm/cchain/vm_test.go` (#5447) + the `cchaintest/blocks.go`
//! builders.

#![allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::sync::Arc;

use ava_chains::atomic::Memory;
use ava_database::MemDb;
use ava_evm_reth::{
    Bytes, EMPTY_OMMER_ROOT_HASH, EMPTY_ROOT_HASH, Header, RethBlock, keccak256, rlp_encode,
};
use ava_saevm_cchain::block_ext::{calc_ext_data_hash, empty_ext_data_hash};
use ava_saevm_cchain::vm::{Error, Vm};
use ava_types::id::Id;

fn avax_asset_id() -> Id {
    Id::from([0x0a; 32])
}

fn c_chain_id() -> Id {
    Id::from([0xc0; 32])
}

fn new_vm() -> Vm {
    let base: Arc<dyn ava_database::DynDatabase> = Arc::new(MemDb::new());
    let memory = Memory::new(Arc::clone(&base));
    let sm = memory.new_shared_memory(c_chain_id());
    Vm::initialize(&base, Arc::new(sm), c_chain_id(), avax_asset_id()).expect("initialize")
}

/// A bare SAE eth block at `number`/`timestamp` with no extData carrier (empty
/// `extra_data`, no trailing RLP item) — the shape every block currently built
/// by the C-Chain takes (no atomic source yet).
fn bare_block_bytes(number: u64, timestamp: u64) -> Vec<u8> {
    let header = Header {
        number,
        timestamp,
        transactions_root: EMPTY_ROOT_HASH,
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        ..Header::default()
    };
    rlp_encode(RethBlock::uncle(header))
}

/// A C-Chain block that commits `CalcExtDataHash(ext_data)` in its header's
/// `extra_data` and carries the `BlockBodyExtra` as the trailing RLP items
/// `[version, ext_data]` (the approach-(B) carrier; `version` precedes
/// `ext_data`, matching Go's `[Header, Txs, Uncles, Version, ExtData]` field
/// order). When `committed` differs from the real `ext_data`, the block is
/// tampered (header commitment unchanged). `version` simulates a
/// `BlockBodyExtra.Version` (Go `WithBlockVersion`); only `0` is accepted.
fn versioned_block_bytes(
    version: u32,
    committed: &[u8],
    ext_data: &[u8],
    number: u64,
    timestamp: u64,
) -> Vec<u8> {
    let hash = calc_ext_data_hash(committed);
    let header = Header {
        number,
        timestamp,
        transactions_root: EMPTY_ROOT_HASH,
        ommers_hash: EMPTY_OMMER_ROOT_HASH,
        extra_data: Bytes::copy_from_slice(hash.as_slice()),
        ..Header::default()
    };
    let mut wire = rlp_encode(RethBlock::uncle(header));
    wire.extend_from_slice(&rlp_encode(version));
    wire.extend_from_slice(&rlp_encode(ext_data));
    wire
}

/// A well-formed (version-0) committed block — the common case.
fn committed_block_bytes(
    committed: &[u8],
    ext_data: &[u8],
    number: u64,
    timestamp: u64,
) -> Vec<u8> {
    versioned_block_bytes(0, committed, ext_data, number, timestamp)
}

#[test]
fn calc_ext_data_hash_empty_matches_canonical_constant() {
    // Go `CalcExtDataHash(nil) == EmptyExtDataHash == keccak256(RLP(nil))`.
    assert_eq!(
        calc_ext_data_hash(&[]),
        empty_ext_data_hash(),
        "empty extData hashes to EmptyExtDataHash"
    );
    // The empty special-case is just an optimization: the general keccak256(RLP)
    // path yields the same value (RLP("") == 0x80).
    let empty: &[u8] = &[];
    assert_eq!(
        empty_ext_data_hash(),
        keccak256(rlp_encode(empty)),
        "EmptyExtDataHash == keccak256(rlp(empty))"
    );
}

#[test]
fn calc_ext_data_hash_nonempty_is_keccak_of_rlp() {
    let data = b"some-marshaled-atomic-txs";
    assert_eq!(
        calc_ext_data_hash(data),
        keccak256(rlp_encode(&data[..])),
        "CalcExtDataHash(x) == keccak256(rlp(x))"
    );
    assert_ne!(
        calc_ext_data_hash(data),
        empty_ext_data_hash(),
        "non-empty extData does not collide with the empty hash"
    );
}

#[tokio::test]
async fn parse_block_accepts_well_formed_committed_block() {
    let vm = new_vm();
    let ext_data = b"atomic-import-export-bytes";
    let bytes = committed_block_bytes(ext_data, ext_data, 1, 1);
    let block = vm
        .parse_block(&bytes)
        .expect("well-formed committed block parses");
    assert_eq!(block.block().height(), 1, "parsed block is at height 1");
}

#[tokio::test]
async fn parse_block_rejects_tampered_ext_data() {
    let vm = new_vm();
    // Header commits CalcExtDataHash("atomic-A"); the trailing body is a
    // different "atomic-B" — the block ID (header hash) is unchanged.
    let bytes = committed_block_bytes(b"atomic-A", b"atomic-B-tampered", 1, 1);
    match vm.parse_block(&bytes) {
        Err(Error::ExtDataHashMismatch { .. }) => {}
        Err(other) => panic!("expected ExtDataHashMismatch, got {other:?}"),
        Ok(_) => panic!("tampered extData was not rejected"),
    }
}

#[tokio::test]
async fn parse_block_rejects_invalid_version() {
    // Mirrors Go `TestParseBlock`/`invalid_version`: a block whose
    // `BlockBodyExtra.Version` is non-zero is rejected before the extData-hash
    // check. The header commits neither the Version nor the extData, so the
    // block ID is unchanged — `parse_block` is the boundary that catches it.
    let vm = new_vm();
    let ext_data = b"atomic-import-export-bytes";
    let bytes = versioned_block_bytes(1, ext_data, ext_data, 1, 1);
    match vm.parse_block(&bytes) {
        Err(Error::InvalidBlockVersion(1)) => {}
        Err(other) => panic!("expected InvalidBlockVersion(1), got {other:?}"),
        Ok(_) => panic!("non-zero block version was not rejected"),
    }
}

#[tokio::test]
async fn parse_block_rejects_invalid_version_before_ext_data_hash() {
    // The version check is unconditional and precedes the extData-hash check
    // (Go ordering): a block with both a non-zero version AND a tampered extData
    // is rejected for the version, not the hash mismatch.
    let vm = new_vm();
    let bytes = versioned_block_bytes(2, b"atomic-A", b"atomic-B-tampered", 1, 1);
    match vm.parse_block(&bytes) {
        Err(Error::InvalidBlockVersion(2)) => {}
        Err(other) => panic!("expected InvalidBlockVersion(2) to take precedence, got {other:?}"),
        Ok(_) => panic!("non-zero block version was not rejected"),
    }
}

#[tokio::test]
async fn parse_block_accepts_bare_block_without_commitment() {
    // A stock SAE block (empty extra_data, no trailing extData) carries no
    // ExtDataHash commitment — Go's pre-AP1 TODO analog. It must still parse
    // (the verification boundary is dormant until the build path commits, M7.22).
    let vm = new_vm();
    let bytes = bare_block_bytes(1, 1);
    let block = vm
        .parse_block(&bytes)
        .expect("bare uncommitted block still parses");
    assert_eq!(
        block.block().height(),
        1,
        "parsed bare block is at height 1"
    );
}
