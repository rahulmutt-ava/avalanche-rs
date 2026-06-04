// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Recoverable secp256k1 with consensus-critical low-S enforcement.
//!
//! Wraps the C-FFI `secp256k1` crate behind a safe API (`specs/00` §7.6); add
//! the `secp256k1` dependency (Cargo.toml) when implementing.
//!
//! TODO(M0.18): per `specs/03-core-primitives.md` §3.4 — `SIGNATURE_LEN=65`
//! `[r||s||v]`, `PRIVATE_KEY_LEN=32`, `PUBLIC_KEY_LEN=33`,
//! `PRIVATE_KEY_PREFIX="PrivateKey-"`; ava<->recoverable sig reordering vs decred
//! `[v'||r||s]`; `sign_hash` (RFC6979 + low-S); `verify_sig_format` rejects
//! high-S before recovery; `PublicKey::{bytes, address, eth_address}`;
//! `VerifyHash` recovers + compares addresses.
