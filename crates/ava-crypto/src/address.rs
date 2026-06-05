// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Bech32 chain-prefixed addresses (`X-avax1...`, `P-fuji1...`).
//!
//! Byte-exact port of avalanchego `utils/formatting/address/address.go`. Uses
//! standard **bech32** (NOT bech32m) with `ConvertBits(8, 5, pad=true)` — the
//! `bech32 = "0.11"` crate performs the regrouping internally. Owning spec:
//! `specs/03-core-primitives.md` §3.3, `specs/15` §4.4.

use bech32::primitives::decode::CheckedHrpstring;
use bech32::{Bech32, Hrp};

use crate::error::{Error, Result};

/// The chain-prefix separator (Go `address.addressSep`).
const SEPARATOR: char = '-';

/// `address.formatBech32` — bech32-encode `payload` under `hrp`.
///
/// # Errors
/// Returns [`Error::Bech32`] if the HRP is invalid or encoding fails.
pub fn format_bech32(hrp: &str, payload: &[u8]) -> Result<String> {
    let parsed = Hrp::parse(hrp).map_err(|e| Error::Bech32(e.to_string()))?;
    bech32::encode::<Bech32>(parsed, payload).map_err(|e| Error::Bech32(e.to_string()))
}

/// `address.parseBech32` — decode a bech32 string into `(hrp, 8-bit payload)`.
///
/// # Errors
/// Returns [`Error::Bech32`] if the string is not a valid standard-bech32
/// encoding (e.g. bad checksum, bech32m variant).
pub fn parse_bech32(s: &str) -> Result<(String, Vec<u8>)> {
    let checked = CheckedHrpstring::new::<Bech32>(s).map_err(|e| Error::Bech32(e.to_string()))?;
    let hrp = checked.hrp().to_lowercase();
    let payload: Vec<u8> = checked.byte_iter().collect();
    Ok((hrp, payload))
}

/// `address.Format(chainIDAlias, hrp, addr)` — `"alias-bech32(hrp, addr)"`.
///
/// # Errors
/// Returns [`Error::Bech32`] on encoding failure.
pub fn format(chain_alias: &str, hrp: &str, addr: &[u8]) -> Result<String> {
    let addr_str = format_bech32(hrp, addr)?;
    Ok(format!("{chain_alias}{SEPARATOR}{addr_str}"))
}

/// `address.Parse(addrStr)` — split on the FIRST `-` into `(alias, hrp, bytes)`.
///
/// # Errors
/// - [`Error::NoSeparator`] if there is no `-` separator (Go
///   `address.errNoSeparator`).
/// - [`Error::Bech32`] if the bech32 body is malformed.
pub fn parse(addr: &str) -> Result<(String, String, Vec<u8>)> {
    // Go: strings.SplitN(addrStr, addressSep, 2); require exactly 2 parts.
    let (alias, rest) = addr.split_once(SEPARATOR).ok_or(Error::NoSeparator)?;
    let (hrp, payload) = parse_bech32(rest)?;
    Ok((alias.to_string(), hrp, payload))
}
