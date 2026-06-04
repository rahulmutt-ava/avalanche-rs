// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Big-integer-backed `Bits` set + `Bits64` fast path.
//!
//! TODO(M0.9): `Bits` over `num_bigint::BigUint` (Add/Remove/Contains/Union/
//! Intersection/Difference/Len(popcount)/BitLen/Bytes/from_bytes big-endian,
//! `String` = hex) + a `Bits64` u64 fast path.
//! Owning spec: `specs/03-core-primitives.md` §4.2.
