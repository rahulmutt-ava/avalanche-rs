// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Hashing + address derivation (`utils/hashing`).
//!
//! Byte-exact port of avalanchego `utils/hashing/hashing.go`. Owning spec:
//! `specs/03-core-primitives.md` §3.1.

use ripemd::Ripemd160;
use sha2::{Digest, Sha256};
use sha3::Keccak256;

/// Length of a sha256 hash (Go `hashing.HashLen`).
pub const HASH_LEN: usize = 32;

/// Length of a ripemd160 address (Go `hashing.AddrLen`).
pub const ADDR_LEN: usize = 20;

/// `hashing.ComputeHash256` — sha256 of `b`.
#[must_use]
pub fn sha256(b: &[u8]) -> [u8; HASH_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(b);
    hasher.finalize().into()
}

/// ripemd160 of `b` (Go `ripemd160.New`).
#[must_use]
pub fn ripemd160(b: &[u8]) -> [u8; ADDR_LEN] {
    let mut hasher = Ripemd160::new();
    hasher.update(b);
    hasher.finalize().into()
}

/// keccak256 of `b` (EVM hashing; Go `crypto.Keccak256`).
#[must_use]
pub fn keccak256(b: &[u8]) -> [u8; HASH_LEN] {
    let mut hasher = Keccak256::new();
    hasher.update(b);
    hasher.finalize().into()
}

/// `hashing.Checksum(b, n)` — the LAST `n` bytes of `sha256(b)`.
///
/// # Panics
/// Panics if `n > 32` (Go slices `bytes[len-n:]`, which would panic for `n>32`).
#[must_use]
pub fn checksum(b: &[u8], n: usize) -> Vec<u8> {
    assert!(n <= HASH_LEN, "checksum length {n} exceeds sha256 size 32");
    let h = sha256(b);
    h[HASH_LEN - n..].to_vec()
}

/// `hashing.PubkeyBytesToAddress` — `ripemd160(sha256(key))`.
///
/// This is the Avalanche address scheme; also reused for NodeID derivation over
/// a whole DER cert (`specs/03` §3.6).
#[must_use]
pub fn pubkey_bytes_to_address(key: &[u8]) -> [u8; ADDR_LEN] {
    ripemd160(&sha256(key))
}
