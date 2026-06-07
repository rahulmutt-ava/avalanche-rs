// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! EXIT-GATE golden test `golden::bls_sign_pop` (M0.19).
//!
//! Mirrors avalanchego `utils/crypto/bls/*_test.go`. Vectors are Go-generated:
//! a fixed secret key, its compressed pubkey + PoP, a signed message, and an
//! aggregate over three keys.

use ava_crypto::bls::{
    CIPHERSUITE_POP, CIPHERSUITE_SIGNATURE, PUBLIC_KEY_LEN, PublicKey, SECRET_KEY_LEN,
    SIGNATURE_LEN, SecretKey, Signature, aggregate_public_keys, aggregate_signatures, verify,
    verify_pop,
};

#[derive(serde::Deserialize)]
struct BlsVectors {
    secret_hex: String,
    pub_compressed_hex: String,
    pop_hex: String,
    msg_hex: String,
    sig_hex: String,
    agg_secrets_hex: Vec<String>,
    agg_sig_hex: String,
    agg_pub_compressed_hex: String,
    dst_signature: String,
    dst_pop: String,
}

fn vectors() -> BlsVectors {
    let raw = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/vectors/crypto/bls.json"
    ))
    .expect("read bls.json");
    serde_json::from_str(&raw).expect("parse bls.json")
}

mod golden {
    use super::*;

    #[test]
    fn bls_sign_pop() {
        let v = vectors();

        // DST byte-equality.
        assert_eq!(hex::encode(CIPHERSUITE_SIGNATURE), v.dst_signature);
        assert_eq!(hex::encode(CIPHERSUITE_POP), v.dst_pop);
        assert_eq!(PUBLIC_KEY_LEN, 48);
        assert_eq!(SIGNATURE_LEN, 96);
        assert_eq!(SECRET_KEY_LEN, 32);

        let sk_bytes = hex::decode(&v.secret_hex).expect("decode secret");
        let sk = SecretKey::from_bytes(&sk_bytes).expect("sk from_bytes");
        let pk = sk.public_key();

        // (pk_compressed[48], pop[96]) equals committed Go bytes.
        assert_eq!(hex::encode(pk.compress()), v.pub_compressed_hex);
        let pop = sk.sign_pop(&pk.compress());
        assert_eq!(hex::encode(pop.compress()), v.pop_hex);

        // verify_pop(pk, pop, pk.compress()) accepts.
        assert!(verify_pop(&pk, &pop, &pk.compress()));

        // plain sign + verify over the fixed message accepts.
        let msg = hex::decode(&v.msg_hex).expect("decode msg");
        let sig = sk.sign(&msg);
        assert_eq!(hex::encode(sig.compress()), v.sig_hex);
        assert!(verify(&pk, &sig, &msg));

        // compress / uncompress round-trip.
        let pk_back = PublicKey::from_compressed(&pk.compress()).expect("pk roundtrip");
        assert_eq!(pk_back.compress(), pk.compress());
        let sig_back = Signature::from_bytes(&sig.compress()).expect("sig roundtrip");
        assert_eq!(sig_back.compress(), sig.compress());

        // Cross-verify the Go-produced signature/pubkey from the vector.
        let go_pk =
            PublicKey::from_compressed(&hex::decode(&v.pub_compressed_hex).unwrap()).unwrap();
        let go_sig = Signature::from_bytes(&hex::decode(&v.sig_hex).unwrap()).unwrap();
        assert!(verify(&go_pk, &go_sig, &msg));

        // aggregate(verify) of N sigs over the same message == individual verifies.
        let sks: Vec<SecretKey> = v
            .agg_secrets_hex
            .iter()
            .map(|h| SecretKey::from_bytes(&hex::decode(h).unwrap()).unwrap())
            .collect();
        let pks: Vec<PublicKey> = sks.iter().map(SecretKey::public_key).collect();
        let sigs: Vec<Signature> = sks.iter().map(|s| s.sign(&msg)).collect();

        // Each individual sig verifies.
        for (p, s) in pks.iter().zip(sigs.iter()) {
            assert!(verify(p, s, &msg));
        }

        let pk_refs: Vec<&PublicKey> = pks.iter().collect();
        let sig_refs: Vec<&Signature> = sigs.iter().collect();
        let agg_pk = aggregate_public_keys(&pk_refs).expect("agg pk");
        let agg_sig = aggregate_signatures(&sig_refs).expect("agg sig");

        assert_eq!(hex::encode(agg_pk.compress()), v.agg_pub_compressed_hex);
        assert_eq!(hex::encode(agg_sig.compress()), v.agg_sig_hex);
        // The aggregate signature verifies under the aggregate public key.
        assert!(verify(&agg_pk, &agg_sig, &msg));

        // Empty aggregate -> error.
        assert!(aggregate_public_keys(&[]).is_err());
        assert!(aggregate_signatures(&[]).is_err());
    }

    /// `from_uncompressed` round-trips a key's 96-byte uncompressed serialization
    /// (the form the P-Chain stores in its public-key-diff sublists, M4.21).
    #[test]
    fn public_key_uncompressed_roundtrip() {
        let sk = SecretKey::from_bytes(&[0x11; SECRET_KEY_LEN]).expect("sk");
        let pk = sk.public_key();
        let uncompressed = pk.serialize();
        assert_eq!(uncompressed.len(), 96);

        let back = PublicKey::from_uncompressed(&uncompressed).expect("uncompressed roundtrip");
        assert_eq!(back.compress(), pk.compress());
        assert_eq!(back.serialize(), uncompressed);

        // Garbage bytes are rejected (subgroup check fails).
        assert!(PublicKey::from_uncompressed(&[0xAB; 96]).is_err());
    }
}
