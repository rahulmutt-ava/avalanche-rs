// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Hashing + address derivation (`utils/hashing`).
//!
//! TODO(M0.13): `HASH_LEN=32`, `ADDR_LEN=20`, `sha256` (sha2), `ripemd160`
//! (ripemd), `keccak256` (sha3), `checksum(b,n)` = last n bytes of sha256,
//! `pubkey_bytes_to_address` = `ripemd160(sha256(key))`.
//! Owning spec: `specs/03-core-primitives.md` §3.1.
