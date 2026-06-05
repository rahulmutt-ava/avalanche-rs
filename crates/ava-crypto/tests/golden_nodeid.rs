// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EXIT-GATE golden test `golden::nodeid_from_cert` (M0.20).
//!
//! Mirrors avalanchego `staking/{tls,parse}_test.go` and `ids/node_id.go`.
//! Accept-path vectors are Go-generated ECDSA P-256 staking certs. The
//! `large_rsa_key` REJECT case (RSA-3072) is NOT in the committed vector (see
//! `nodeid.json` `_note`), so it is synthesized in-test: we hand-build a minimal
//! DER X.509 certificate carrying an RSA-3072 `SubjectPublicKeyInfo` (modulus =
//! 3072 random odd bits, exponent = 65537) and assert the strict parser rejects
//! it with [`Error::UnsupportedRsaModulusBitLen`]. The oversize-cert and
//! malformed-DER rejects are also exercised. No new dependency is added — the
//! synthetic DER is built with a tiny local TLV encoder.

use ava_crypto::Error;
use ava_crypto::staking::{MAX_CERTIFICATE_LEN, node_id_from_cert, parse_certificate};

#[derive(serde::Deserialize)]
struct NodeIdCase {
    cert_der_hex: String,
    node_id: String,
}

#[derive(serde::Deserialize)]
struct NodeIdVectors {
    cases: Vec<NodeIdCase>,
}

fn vectors() -> NodeIdVectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/nodeid.json"
    ))
    .expect("read nodeid.json");
    serde_json::from_str(&raw).expect("parse nodeid.json")
}

mod golden {
    use super::*;

    #[test]
    fn nodeid_from_cert() {
        for c in vectors().cases {
            let der = hex::decode(&c.cert_der_hex).expect("decode der");

            // NodeID == ripemd160(sha256(whole DER)), with the NodeID- prefix.
            let node_id = node_id_from_cert(&der);
            let got = format!(
                "NodeID-{}",
                ava_utils::cb58::cb58_encode(node_id.as_bytes()).unwrap()
            );
            assert_eq!(got, c.node_id);

            // The strict parser accepts the P-256 ECDSA cert.
            let cert = parse_certificate(&der).expect("parse accept");
            assert_eq!(cert.raw, der);
        }
    }

    #[test]
    fn rejects_oversize_cert() {
        // A DER blob larger than MAX_CERTIFICATE_LEN is rejected before parsing.
        let big = vec![0x30u8; MAX_CERTIFICATE_LEN + 1];
        assert!(matches!(
            parse_certificate(&big),
            Err(Error::CertificateTooLarge)
        ));
    }

    #[test]
    fn rejects_garbage_der() {
        // Non-certificate bytes (within the size limit) fail to parse.
        let garbage = [0x01u8, 0x02, 0x03, 0x04];
        assert!(parse_certificate(&garbage).is_err());
    }

    // The `large_rsa_key` REJECT case (RSA-3072) is not in the committed vector
    // (nodeid.json `_note`), and `rcgen` (ring backend) cannot generate RSA keys.
    // We synthesize a minimal X.509 cert DER carrying an RSA-3072 SPKI so the
    // strict parser's modulus-bitlen check is exercised end-to-end.
    #[test]
    fn rejects_large_rsa_key() {
        let der = der::rsa_cert(3072, 65537);
        assert!(matches!(
            parse_certificate(&der),
            Err(Error::UnsupportedRsaModulusBitLen)
        ));
    }

    // The wrong-exponent reject: an RSA-2048 (in-policy bitlen) key whose
    // exponent is 3 trips the exponent check.
    #[test]
    fn rejects_bad_rsa_exponent() {
        let der = der::rsa_cert(2048, 3);
        assert!(matches!(
            parse_certificate(&der),
            Err(Error::UnsupportedRsaPublicExponent)
        ));
    }
}

/// A minimal DER X.509 v3 certificate builder, just rich enough for the strict
/// staking parser to reach the `SubjectPublicKeyInfo` and run its RSA policy
/// checks. The modulus is a random odd integer of the requested bit length (it
/// need not be a real RSA modulus — the parser only inspects bit length, parity
/// and the exponent). Not a general-purpose encoder.
mod der {
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        let len = content.len();
        if len < 0x80 {
            out.push(len as u8);
        } else {
            let bytes = len.to_be_bytes();
            let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
            let sig = &bytes[start..];
            out.push(0x80 | (sig.len() as u8));
            out.extend_from_slice(sig);
        }
        out.extend_from_slice(content);
        out
    }

    fn seq(content: &[u8]) -> Vec<u8> {
        tlv(0x30, content)
    }

    fn integer(content: &[u8]) -> Vec<u8> {
        tlv(0x02, content)
    }

    /// A positive big-endian INTEGER content (prepends 0x00 if the top bit set).
    fn positive_int_content(be: &[u8]) -> Vec<u8> {
        let trimmed = {
            let start = be.iter().position(|&b| b != 0).unwrap_or(be.len() - 1);
            &be[start..]
        };
        if trimmed.first().is_some_and(|&b| b & 0x80 != 0) {
            let mut v = vec![0x00];
            v.extend_from_slice(trimmed);
            v
        } else {
            trimmed.to_vec()
        }
    }

    /// OBJECT IDENTIFIER TLV from already-encoded OID content bytes.
    fn oid(content: &[u8]) -> Vec<u8> {
        tlv(0x06, content)
    }

    /// Build a self-contained cert DER with an RSA SPKI of `modulus_bits` bits
    /// and the given public exponent.
    pub fn rsa_cert(modulus_bits: usize, exponent: u64) -> Vec<u8> {
        // Random-ish odd modulus of exactly `modulus_bits` bits: set the top bit
        // and the low bit; fill the middle deterministically.
        let nbytes = modulus_bits / 8;
        let mut modulus = vec![0xa5u8; nbytes];
        modulus[0] |= 0x80; // ensure the top bit -> exactly modulus_bits bits
        let last = nbytes - 1;
        modulus[last] |= 0x01; // odd

        let exp_be = exponent.to_be_bytes();
        let exp_start = exp_be.iter().position(|&b| b != 0).unwrap_or(7);

        // RSAPublicKey ::= SEQUENCE { modulus INTEGER, publicExponent INTEGER }
        let rsa_pubkey = {
            let mut body = integer(&positive_int_content(&modulus));
            body.extend_from_slice(&integer(&positive_int_content(&exp_be[exp_start..])));
            seq(&body)
        };

        // AlgorithmIdentifier { rsaEncryption (1.2.840.113549.1.1.1), NULL }
        let rsa_oid = oid(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]);
        let alg_id = {
            let mut body = rsa_oid.clone();
            body.extend_from_slice(&tlv(0x05, &[])); // NULL
            seq(&body)
        };

        // SubjectPublicKeyInfo ::= SEQUENCE { algorithm, subjectPublicKey BIT STRING }
        let spki = {
            let mut bitstr_content = vec![0x00]; // 0 unused bits
            bitstr_content.extend_from_slice(&rsa_pubkey);
            let mut body = alg_id.clone();
            body.extend_from_slice(&tlv(0x03, &bitstr_content));
            seq(&body)
        };

        // sha256WithRSAEncryption (1.2.840.113549.1.1.11)
        let sig_alg = {
            let mut body = oid(&[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]);
            body.extend_from_slice(&tlv(0x05, &[])); // NULL
            seq(&body)
        };

        // Minimal empty Name ::= SEQUENCE OF {} (issuer / subject).
        let empty_name = seq(&[]);

        // Validity ::= SEQUENCE { notBefore UTCTime, notAfter UTCTime }
        let validity = {
            let nb = tlv(0x17, b"991231000000Z");
            let na = tlv(0x18, b"21260604221506Z");
            let mut body = nb;
            body.extend_from_slice(&na);
            seq(&body)
        };

        // TBSCertificate: [0] version v3(2), serial 0, sigAlg, issuer, validity,
        // subject, SPKI.
        let version = tlv(0xa0, &integer(&[0x02]));
        let serial = integer(&[0x00]);
        let tbs = {
            let mut body = version;
            body.extend_from_slice(&serial);
            body.extend_from_slice(&sig_alg);
            body.extend_from_slice(&empty_name);
            body.extend_from_slice(&validity);
            body.extend_from_slice(&empty_name);
            body.extend_from_slice(&spki);
            seq(&body)
        };

        // Certificate ::= SEQUENCE { tbs, sigAlg, signature BIT STRING }
        let signature = tlv(0x03, &[0x00, 0xde, 0xad, 0xbe, 0xef]);
        let mut body = tbs;
        body.extend_from_slice(&sig_alg);
        body.extend_from_slice(&signature);
        seq(&body)
    }
}
