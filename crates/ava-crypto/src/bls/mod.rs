// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS12-381 (`min_pk`) — sign / aggregate / proof-of-possession / verify,
//! plus the `Signer` trait and `LocalSigner` lifecycle.
//!
//! Wraps the C-FFI `blst` crate behind a safe API (`specs/00` §7.6). All
//! `unsafe` is encapsulated inside `blst`.
//!
//! Submodules:
//! - [`ciphersuite`] — the SIGNATURE / POP DST byte strings (M0.19)
//! - [`keys`] — `SecretKey` / `PublicKey` (compress/uncompress + subgroup check) (M0.19)
//! - [`sign`] — `Signature`, aggregate, `verify` / `verify_pop` (M0.19)
//! - [`signer`] — object-safe `Signer` trait (M0.21)
//! - [`local_signer`] — file-backed `LocalSigner` (zeroize, 0o400) (M0.21)

pub mod ciphersuite;
pub mod keys;
pub mod local_signer;
pub mod sign;
pub mod signer;

pub use ciphersuite::{CIPHERSUITE_POP, CIPHERSUITE_SIGNATURE};
pub use keys::{
    PUBLIC_KEY_LEN, PublicKey, SECRET_KEY_LEN, SecretKey, UNCOMPRESSED_PUBLIC_KEY_LEN,
    aggregate_public_keys,
};
pub use local_signer::LocalSigner;
pub use sign::{SIGNATURE_LEN, Signature, aggregate_signatures, verify, verify_pop};
pub use signer::Signer;
