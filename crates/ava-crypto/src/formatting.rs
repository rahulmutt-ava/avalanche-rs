// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Payload encodings — Hex / HexC / HexNc / Json (`utils/formatting`).
//!
//! Byte-exact port of avalanchego `utils/formatting/encoding.go`. Owning spec:
//! `specs/03-core-primitives.md` §3.2.
//!
//! - `Hex` / `HexC` = `"0x" + hex(payload ++ checksum4)` (4-byte sha256-tail).
//! - `HexNc` = `"0x" + hex(payload)` (no checksum).
//! - `Json` is unsupported on this call path (Go returns an error here).

use crate::error::{Error, Result};
use crate::hashing::checksum;

/// Number of checksum bytes appended in the checksummed encodings.
const CHECKSUM_LEN: usize = 4;

/// The `0x` prefix required by the hex encodings.
const HEX_PREFIX: &str = "0x";

/// Payload encodings (Go `formatting.Encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// `"0x" + hex(payload ++ checksum4)`. The default API encoding.
    Hex,
    /// `"0x" + hex(payload)` — no checksum (Go `HexNC`).
    HexNc,
    /// `"0x" + hex(payload ++ checksum4)` — same wire form as [`Encoding::Hex`]
    /// (Go `HexC`).
    HexC,
    /// JSON — unsupported on the byte-encode/decode path (Go errors here).
    Json,
}

/// `formatting.Encode(encoding, bytes)`.
///
/// # Errors
/// Returns [`Error::UnsupportedEncoding`] for [`Encoding::Json`].
pub fn encode(encoding: Encoding, payload: &[u8]) -> Result<String> {
    match encoding {
        Encoding::HexNc => Ok(format!("{HEX_PREFIX}{}", hex::encode(payload))),
        Encoding::Hex | Encoding::HexC => {
            let mut checked = payload.to_vec();
            checked.extend_from_slice(&checksum(payload, CHECKSUM_LEN));
            Ok(format!("{HEX_PREFIX}{}", hex::encode(checked)))
        }
        Encoding::Json => Err(Error::UnsupportedEncoding),
    }
}

/// `formatting.Decode(encoding, str)`.
///
/// Requires the `0x` prefix; for the checksummed encodings, verifies and strips
/// the trailing 4-byte checksum.
///
/// # Errors
/// - [`Error::MissingHexPrefix`] if the `0x` prefix is absent.
/// - [`Error::HexDecoding`] if the hex body is malformed.
/// - [`Error::MissingChecksum`] if the payload is shorter than the checksum.
/// - [`Error::BadChecksum`] if the trailing checksum does not verify.
/// - [`Error::UnsupportedEncoding`] for [`Encoding::Json`].
pub fn decode(encoding: Encoding, s: &str) -> Result<Vec<u8>> {
    if encoding == Encoding::Json {
        return Err(Error::UnsupportedEncoding);
    }
    // Go treats the empty string as empty bytes regardless of prefix.
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let body = s
        .strip_prefix(HEX_PREFIX)
        .ok_or(Error::MissingHexPrefix)?;
    let decoded = hex::decode(body).map_err(|e| Error::HexDecoding(e.to_string()))?;

    match encoding {
        Encoding::HexNc => Ok(decoded),
        Encoding::Hex | Encoding::HexC => {
            if decoded.len() < CHECKSUM_LEN {
                return Err(Error::MissingChecksum);
            }
            let split = decoded.len() - CHECKSUM_LEN;
            let (raw, ck) = decoded.split_at(split);
            if ck != checksum(raw, CHECKSUM_LEN).as_slice() {
                return Err(Error::BadChecksum);
            }
            Ok(raw.to_vec())
        }
        Encoding::Json => Err(Error::UnsupportedEncoding),
    }
}
