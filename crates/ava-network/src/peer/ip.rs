// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Signed-IP claim types (`specs/05` §1.6, `specs/15` §4.1).
//!
//! Mirrors Go `network/peer/ip.go`. A validator claims its `(ip, port)` at a
//! `timestamp`; the claim is signed both with the staking TLS key (over
//! `SHA256(bytes)`) and with the BLS key as a proof-of-possession (over the raw
//! `bytes`). The signed bytes layout is:
//!
//! ```text
//! ip.As16() (16) || port (u16 BE) || timestamp (u64 BE)   == 26 bytes
//! ```
//!
//! `As16()` of an IPv4 address is its IPv4-mapped IPv6 form
//! (`00..00 ffff a.b.c.d`), matching Go `netip.Addr.As16()`.

use std::net::IpAddr;

use ava_crypto::bls::Signer;
use ava_crypto::staking::{Certificate, check_signature};
use ring::rand::SystemRandom;
use ring::signature::EcdsaKeyPair;

use crate::error::{Error, Result};

/// Length of the signed-IP byte layout: 16 (As16) + 2 (port) + 8 (timestamp).
pub const UNSIGNED_IP_LEN: usize = 16 + 2 + 8;

/// An unsigned IP claim: an address/port plus the claim timestamp.
///
/// `Timestamp` ensures peers track the most-recent claim for a validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsignedIp {
    /// The claimed IP address.
    pub ip: IpAddr,
    /// The claimed port.
    pub port: u16,
    /// The Unix-seconds timestamp of the claim.
    pub timestamp: u64,
}

impl UnsignedIp {
    /// Construct an unsigned IP claim.
    #[must_use]
    pub fn new(ip: IpAddr, port: u16, timestamp: u64) -> UnsignedIp {
        UnsignedIp {
            ip,
            port,
            timestamp,
        }
    }

    /// The 16-byte `As16()` form of the address: IPv6 octets verbatim, or the
    /// IPv4-mapped IPv6 form for an IPv4 address (`00..00 ffff a.b.c.d`).
    #[must_use]
    pub fn addr_as16(&self) -> [u8; 16] {
        match self.ip {
            IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
            IpAddr::V6(v6) => v6.octets(),
        }
    }

    /// The signed byte layout: `As16()(16) || port_be(2) || timestamp_be(8)`.
    ///
    /// Mirrors Go `UnsignedIP.bytes()` (`wrappers.Packer` PackFixedBytes /
    /// PackShort / PackLong).
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(UNSIGNED_IP_LEN);
        out.extend_from_slice(&self.addr_as16());
        out.extend_from_slice(&self.port.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out
    }

    /// Sign this claim, producing a [`SignedIp`].
    ///
    /// The TLS signature is an ECDSA-P256/SHA-256 signature whose digest is
    /// `SHA256(bytes)` (the `ring` signer hashes the message internally, exactly
    /// matching Go signing the pre-computed `crypto.SHA256(ipBytes)` digest).
    /// The BLS signature is a proof-of-possession over the raw `bytes`.
    ///
    /// # Errors
    /// [`Error::Signing`] if either the TLS or BLS signing operation fails.
    pub fn sign(&self, tls_signer: &EcdsaKeyPair, bls_signer: &dyn Signer) -> Result<SignedIp> {
        let ip_bytes = self.bytes();

        let rng = SystemRandom::new();
        let tls_signature = tls_signer
            .sign(&rng, &ip_bytes)
            .map_err(|_| Error::Signing("tls sign failed".into()))?
            .as_ref()
            .to_vec();

        let bls_signature = bls_signer
            .sign_proof_of_possession(&ip_bytes)
            .map_err(|e| Error::Signing(format!("bls pop sign failed: {e}")))?;
        let bls_signature_bytes = bls_signature.compress().to_vec();

        Ok(SignedIp {
            unsigned: *self,
            tls_signature,
            bls_signature_bytes,
        })
    }
}

/// A [`UnsignedIp`] plus its TLS + BLS signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedIp {
    /// The signed claim.
    pub unsigned: UnsignedIp,
    /// ECDSA-P256/SHA-256 (ASN.1/DER) signature over `SHA256(unsigned.bytes())`.
    pub tls_signature: Vec<u8>,
    /// Compressed BLS proof-of-possession signature over `unsigned.bytes()`.
    pub bls_signature_bytes: Vec<u8>,
}

impl SignedIp {
    /// The compressed BLS proof-of-possession signature bytes.
    #[must_use]
    pub fn bls_signature_bytes(&self) -> &[u8] {
        &self.bls_signature_bytes
    }

    /// The TLS signature bytes.
    #[must_use]
    pub fn tls_signature(&self) -> &[u8] {
        &self.tls_signature
    }

    /// Verify the claim. Returns `Ok(())` iff:
    /// - `unsigned.timestamp <= max_timestamp` (= now + 60s upstream), and
    /// - `tls_signature` is a valid signature over `unsigned.bytes()` under
    ///   `cert` (i.e. over `SHA256(bytes)`).
    ///
    /// Mirrors Go `SignedIP.Verify`.
    ///
    /// # Errors
    /// [`Error::TimestampTooFarInFuture`] if the timestamp exceeds `max_timestamp`;
    /// [`Error::InvalidTlsSignature`] if the TLS signature does not verify.
    pub fn verify(&self, cert: &Certificate, max_timestamp: u64) -> Result<()> {
        if self.unsigned.timestamp > max_timestamp {
            return Err(Error::TimestampTooFarInFuture);
        }
        check_signature(cert, &self.unsigned.bytes(), &self.tls_signature)
            .map_err(|_| Error::InvalidTlsSignature)
    }

    /// Test-only: flip a byte of the TLS signature so verification fails.
    #[doc(hidden)]
    pub fn corrupt_tls_signature_for_test(&mut self) {
        if let Some(b) = self.tls_signature.last_mut() {
            *b ^= 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    #[derive(serde::Deserialize)]
    struct Layout {
        bytes_hex: String,
    }

    #[test]
    fn ipv4_mapped_as16_and_byte_layout() {
        // 1.2.3.4:9651 @ ts=1_600_000_000 — the documented As16/port/ts layout.
        let unsigned = UnsignedIp::new(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)), 9651, 1_600_000_000);
        let expect = "00000000000000000000ffff0102030425b3000000005f5e1000";
        // exercise hex + serde_json in the lib-test target.
        let parsed: Layout =
            serde_json::from_str(&format!("{{\"bytes_hex\":\"{expect}\"}}")).expect("json");
        assert_eq!(hex::encode(unsigned.bytes()), parsed.bytes_hex);
        assert_eq!(unsigned.bytes().len(), UNSIGNED_IP_LEN);
    }
}
