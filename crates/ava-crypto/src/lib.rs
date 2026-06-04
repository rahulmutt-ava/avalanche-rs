// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-crypto` — hashing, encodings, signatures, and staking certificates.
//!
//! Tier T0 (primitives). Owning specs: `specs/03-core-primitives.md` §3 and
//! `specs/25-key-management-and-signing.md`. Implemented across M0:
//!
//! - [`hashing`] — sha256 / ripemd160 / keccak256 / checksum / address (M0.13)
//! - [`cb58`] — re-export of `ava_utils::cb58` (M0.17)
//! - [`formatting`] — Hex / HexC / HexNC encodings (M0.17)
//! - [`address`] — bech32 chain-prefixed addresses (M0.17)
//! - [`secp256k1`] — recoverable secp256k1, low-S enforce (M0.18)
//! - [`bls`] — BLS12-381 min_pk sign/agg/PoP + Signer/LocalSigner (M0.19, M0.21)
//! - [`staking`] — cert gen + strict parse + NodeID-from-cert (M0.20)
//! - [`error`] — the crate error enum
//!
//! NOTE: this crate does NOT blanket-`forbid(unsafe_code)` — the [`secp256k1`]
//! and [`bls`] modules wrap C FFI bindings (`secp256k1`, `blst`) behind safe
//! APIs with localized `// SAFETY:` notes (`specs/00` §7.6). All other modules
//! contain no `unsafe`.
//!
//! Modules are scaffolded empty in M0.1 and filled in by their owning tasks.

pub mod address;
pub mod bls;
pub mod cb58;
pub mod error;
pub mod formatting;
pub mod hashing;
pub mod secp256k1;
pub mod staking;
