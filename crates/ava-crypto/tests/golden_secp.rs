// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EXIT-GATE golden test `golden::secp_recover` (M0.18).
//!
//! Mirrors avalanchego `utils/crypto/secp256k1/{rfc6979,secp256k1}_test.go`.
//! Vectors are Go-generated: deterministic RFC6979 signatures over a 32-byte
//! hash, the recovered address, the eth address, and a hand-mutated high-S sig.

use ava_crypto::secp256k1::{PrivateKey, PublicKey, Signature, PRIVATE_KEY_PREFIX, SIGNATURE_LEN};

#[derive(serde::Deserialize)]
struct SecpCase {
    priv_hex: String,
    priv_string: String,
    pub_compressed_hex: String,
    address_hex: String,
    eth_address_hex: String,
    hash_hex: String,
    sig_hex: String,
    high_s_sig_hex: String,
}

fn cases() -> Vec<SecpCase> {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/secp.json"
    ))
    .expect("read secp.json");
    serde_json::from_str(&raw).expect("parse secp.json")
}

mod golden {
    use super::*;

    #[test]
    fn secp_recover() {
        for c in cases() {
            let sk_bytes = hex::decode(&c.priv_hex).expect("decode priv");
            let hash = hex::decode(&c.hash_hex).expect("decode hash");
            let mut hash32 = [0u8; 32];
            hash32.copy_from_slice(&hash);

            let sk = PrivateKey::from_bytes(&sk_bytes).expect("priv from_bytes");

            // PrivateKey-CB58 string round-trip.
            assert_eq!(sk.to_string(), c.priv_string);
            assert!(c.priv_string.starts_with(PRIVATE_KEY_PREFIX));
            let sk2 = c.priv_string.parse::<PrivateKey>().expect("priv parse");
            assert_eq!(sk2.to_bytes(), sk.to_bytes());

            // Public key + addresses.
            let pk = sk.public_key();
            assert_eq!(hex::encode(pk.bytes()), c.pub_compressed_hex);
            assert_eq!(hex::encode(pk.address().to_bytes()), c.address_hex);
            assert_eq!(hex::encode(pk.eth_address()), c.eth_address_hex);

            // Deterministic sign matches the committed [r||s||v] vector.
            let sig = sk.sign_hash(&hash32).expect("sign");
            assert_eq!(hex::encode(sig), c.sig_hex);

            // Recover the public key from [r||s||v] and check the address.
            let sig_bytes = hex::decode(&c.sig_hex).expect("decode sig");
            let mut sig65 = [0u8; SIGNATURE_LEN];
            sig65.copy_from_slice(&sig_bytes);
            let recovered = PublicKey::recover_from_hash(&hash32, &sig65).expect("recover");
            assert_eq!(recovered.address(), pk.address());

            // VerifyHash recovers + compares addresses.
            assert!(pk.verify_hash(&hash32, &sig65));

            // High-S signature is rejected BEFORE recovery (consensus-critical).
            let high = hex::decode(&c.high_s_sig_hex).expect("decode high-s");
            let mut high65 = [0u8; SIGNATURE_LEN];
            high65.copy_from_slice(&high);
            assert!(Signature::verify_format(&high65).is_err());
            assert!(PublicKey::recover_from_hash(&hash32, &high65).is_err());
        }
    }
}
