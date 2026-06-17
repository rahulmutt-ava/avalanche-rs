// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! C-Chain block `extData` hash commitment (specs/11 §8 + `10` §9
//! upstream-delta; Go `plugin/evm/customtypes/block_ext.go::CalcExtDataHash` +
//! `hashes_ext.go::EmptyExtDataHash`).
//!
//! A C-Chain block carries its atomic Import/Export txs as `extData`, and
//! commits `keccak256(RLP(extData))` into the header so the block ID (the header
//! hash) covers the `extData` body. [`crate::vm::Vm::parse_block`] recomputes
//! this hash and rejects a block whose `extData` does not match the committed
//! value — the M7.37 verification boundary.

use ava_evm_reth::{B256, keccak256, rlp_encode};

/// `customtypes.EmptyExtDataHash` = `keccak256(RLP(nil))` — the `ExtDataHash` of
/// a block carrying no atomic txs (coreth `hashes_ext.go`). Equal to
/// `keccak256(0x80)` (the RLP encoding of the empty byte string).
pub const EMPTY_EXT_DATA_HASH: [u8; 32] = [
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
];

/// The canonical empty-`extData` hash ([`EMPTY_EXT_DATA_HASH`] as a [`B256`]).
#[must_use]
pub fn empty_ext_data_hash() -> B256 {
    B256::from(EMPTY_EXT_DATA_HASH)
}

/// Port of Go `customtypes.CalcExtDataHash`: `keccak256(RLP(extData))`, with the
/// empty input short-circuiting to [`EMPTY_EXT_DATA_HASH`].
///
/// The short-circuit is purely an optimization — `RLP("")` is `0x80`, so the
/// general path yields the same value for an empty slice.
#[must_use]
pub fn calc_ext_data_hash(ext_data: &[u8]) -> B256 {
    if ext_data.is_empty() {
        return empty_ext_data_hash();
    }
    keccak256(rlp_encode(ext_data))
}
