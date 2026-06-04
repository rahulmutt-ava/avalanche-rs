// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS `Signature` + aggregation + verification.
//!
//! TODO(M0.19): `SIGNATURE_LEN=96`; `Signature::{compress->96, from_bytes
//! (uncompress + sig_validate)}`, `aggregate_signatures` (error on empty),
//! `verify`/`verify_pop` (pass `false` validation flags — validated on parse).
//! Owning spec: `specs/03-core-primitives.md` §3.5.
