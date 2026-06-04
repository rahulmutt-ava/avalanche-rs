// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS `SecretKey` / `PublicKey` (`blst::min_pk`).
//!
//! TODO(M0.19): `PUBLIC_KEY_LEN=48`, `SECRET_KEY_LEN=32`; `SecretKey::{new,
//! from_bytes (zeroize on drop)}`, `sign`/`sign_pop`; `PublicKey::{compress->48,
//! from_compressed (uncompress + key_validate subgroup check), serialize->96}`;
//! `aggregate_public_keys` (error on empty).
//! Owning spec: `specs/03-core-primitives.md` §3.5.
