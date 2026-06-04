// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! CB58 codec (base58 + 4-byte sha256 checksum). Shared by ava-types & ava-crypto.
//!
//! Placed here (not ava-crypto) to break the ava-types -> ava-crypto cycle
//! (plan M0.6/M0.11); the checksum uses `sha2::Sha256` directly.
//!
//! TODO(M0.11): `cb58_encode(b) = bs58(b ++ last4(sha256(b)))` and `cb58_decode`
//! verifying the trailing 4-byte checksum. Bitcoin alphabet, raw bs58 (NOT
//! with_check). Errors `BadChecksum` / `MissingChecksum` / `EncodingOverflow`.
//! Owning spec: `specs/03-core-primitives.md` §3.2, `specs/15` §4.4.
