// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! CB58 codec (base58 + 4-byte sha256 checksum). Shared by ava-types & ava-crypto.
//!
//! Placed here (not ava-crypto) to break the ava-types -> ava-crypto cycle
//! (plan M0.6/M0.11); the checksum uses `sha2::Sha256` directly.
//!
//! `cb58_encode(b) = bs58(b ++ last4(sha256(b)))`; `cb58_decode` reverses it,
//! verifying the trailing 4-byte checksum. Uses the Bitcoin base58 alphabet via
//! raw `bs58` (NOT `with_check`, which is double-sha256 Bitcoin-style — CB58 is a
//! single-sha256 tail).
//! Owning spec: `specs/03-core-primitives.md` §3.2, `specs/15` §4.4.

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Length of the CB58 checksum (Go `cb58.go` `ChecksumLength`).
const CHECKSUM_LEN: usize = 4;

/// Returns the last `n` bytes of `sha256(b)` (Go `hashing.Checksum`).
fn checksum(b: &[u8], n: usize) -> Vec<u8> {
    let h = Sha256::digest(b);
    h[h.len() - n..].to_vec()
}

/// Encodes `b` as a CB58 string: `bs58(b ++ last4(sha256(b)))`.
///
/// # Errors
/// Returns [`Error::EncodingOverflow`] if `b` is too large to append a 4-byte
/// checksum without overflowing the Go `int32` length bound.
pub fn cb58_encode(b: &[u8]) -> Result<String> {
    // Go rejects len > math.MaxInt32 - ChecksumLength (errEncodingOverFlow).
    if b.len() > (i32::MAX as usize) - CHECKSUM_LEN {
        return Err(Error::EncodingOverflow);
    }
    let mut checked = b.to_vec();
    checked.extend_from_slice(&checksum(b, CHECKSUM_LEN));
    Ok(bs58::encode(&checked).into_string())
}

/// Decodes a CB58 string, verifying and stripping the trailing 4-byte checksum.
///
/// # Errors
/// - [`Error::Base58Decoding`] if the string is not valid base58.
/// - [`Error::MissingChecksum`] if the decoded payload is shorter than 4 bytes.
/// - [`Error::BadChecksum`] if the trailing checksum does not match.
pub fn cb58_decode(s: &str) -> Result<Vec<u8>> {
    let decoded = bs58::decode(s)
        .into_vec()
        .map_err(|e| Error::Base58Decoding(e.to_string()))?;
    if decoded.len() < CHECKSUM_LEN {
        return Err(Error::MissingChecksum);
    }
    let (raw, ck) = decoded.split_at(decoded.len() - CHECKSUM_LEN);
    if ck != checksum(raw, CHECKSUM_LEN).as_slice() {
        return Err(Error::BadChecksum);
    }
    Ok(raw.to_vec())
}
