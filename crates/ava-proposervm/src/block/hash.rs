// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `sha256` helpers (Go `hashing.ComputeHash256Array`).

use ava_types::id::Id;
use sha2::{Digest, Sha256};

/// `sha256(data)` packed into an [`Id`] (32-byte digest).
#[must_use]
pub fn sha256_id(data: &[u8]) -> Id {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Id::from(out)
}
