// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-utils` error enum (`thiserror`).
//!
//! TODO(M0.9): seed `Error { Overflow, Underflow }` for checked arithmetic.
//! TODO(M0.11): extend with `Base58Decoding, BadChecksum, MissingChecksum,
//! EncodingOverflow` for the CB58 codec.
//! Owning spec: `specs/03-core-primitives.md` §7.
