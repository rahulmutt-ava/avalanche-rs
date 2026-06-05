// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Property tests for `ava-crypto` (M0.24, `specs/02` §4.1–§4.3).
//!
//! Covers the §4.2 row for `ava-crypto`: sign→verify always accepts; tampered
//! message/sig always rejects; BLS `aggregate(verify) == individual verifies`;
//! secp256k1 malleability rules match Go (low-S).
//!
//! Keys are derived inside each test body from arbitrary 32-byte seeds. The key
//! types (`PrivateKey`, `BlsSecretKey`) do not implement `Debug`, so they cannot
//! be carried directly as proptest inputs (shrinking needs `Debug`); the seeds
//! (`[u8; 32]`) are the shrinkable inputs and invalid scalars are filtered with
//! `prop_filter`, so every test body sees a valid key.

use ava_crypto::bls::{
    SecretKey as BlsSecretKey, aggregate_public_keys, aggregate_signatures, verify as bls_verify,
};
use ava_crypto::hashing::pubkey_bytes_to_address;
use ava_crypto::secp256k1::{PrivateKey, PublicKey, SIGNATURE_LEN, Signature};
use proptest::prelude::*;

/// A 32-byte seed that is a valid secp256k1 scalar. Invalid scalars (zero /
/// >= curve order) are vanishingly rare for random bytes and are filtered out.
fn arb_secp_seed() -> impl Strategy<Value = [u8; 32]> {
    any::<[u8; 32]>().prop_filter("invalid secp scalar", |seed| {
        PrivateKey::from_bytes(seed).is_ok()
    })
}

/// A 32-byte IKM seed that yields a valid BLS secret key.
fn arb_bls_seed() -> impl Strategy<Value = [u8; 32]> {
    any::<[u8; 32]>().prop_filter("invalid bls ikm", |seed| BlsSecretKey::new(seed).is_ok())
}

mod prop {
    use super::*;

    proptest! {
        /// sign→verify always accepts and the recovered address matches the
        /// signer's address.
        #[test]
        fn secp_sign_verify_roundtrip(hash in any::<[u8; 32]>(), seed in arb_secp_seed()) {
            let sk = PrivateKey::from_bytes(&seed).expect("valid scalar");
            let pk = sk.public_key();
            let sig = sk.sign_hash(&hash).expect("sign");

            // verify_hash recovers + compares addresses.
            prop_assert!(pk.verify_hash(&hash, &sig));

            // The recovered public key has the same address as the signer.
            let recovered = PublicKey::recover_from_hash(&hash, &sig).expect("recover");
            prop_assert_eq!(recovered.address(), pk.address());
        }

        /// Flipping a bit of the message hash makes verification fail (the
        /// signer's address no longer matches what the tampered hash recovers).
        #[test]
        fn secp_tamper_rejects(
            hash in any::<[u8; 32]>(),
            seed in arb_secp_seed(),
            byte_idx in 0usize..32,
            bit_idx in 0u32..8,
        ) {
            let sk = PrivateKey::from_bytes(&seed).expect("valid scalar");
            let pk = sk.public_key();
            let sig = sk.sign_hash(&hash).expect("sign");

            // Tamper with one bit of the message hash.
            let mut tampered = hash;
            tampered[byte_idx] ^= 1u8 << bit_idx;

            // The tampered hash must not verify against the original signer.
            // Recovery may succeed (yielding a *different* address) or fail; in
            // either case verify_hash must return false.
            prop_assert!(!pk.verify_hash(&tampered, &sig));
        }

        /// Every signature produced by `sign_hash` has low-S, matching Go's
        /// malleability rule (`verify_format` accepts low-S, rejects high-S).
        #[test]
        fn secp_low_s_property(hash in any::<[u8; 32]>(), seed in arb_secp_seed()) {
            let sk = PrivateKey::from_bytes(&seed).expect("valid scalar");
            let sig = sk.sign_hash(&hash).expect("sign");
            // verify_format returns Err(MutatedSig) for high-S; Ok for low-S.
            prop_assert!(Signature::verify_format(&sig).is_ok());

            // Flipping the S scalar to high-S (s' = order - s) must be rejected,
            // demonstrating the rule is actually enforced, not vacuous.
            if let Some(high) = high_s_variant(&sig) {
                prop_assert!(Signature::verify_format(&high).is_err());
            }
        }

        /// arbitrary message + secret key → sign → verify accepts; tampering the
        /// message rejects.
        #[test]
        fn bls_sign_verify_roundtrip(
            msg in proptest::collection::vec(any::<u8>(), 0..128),
            seed in arb_bls_seed(),
            extra in any::<u8>(),
        ) {
            let sk = BlsSecretKey::new(&seed).expect("valid ikm");
            let pk = sk.public_key();
            let sig = sk.sign(&msg);
            prop_assert!(bls_verify(&pk, &sig, &msg));

            // Tamper: appending a byte changes the message, so verify must fail.
            let mut tampered = msg.clone();
            tampered.push(extra);
            prop_assert!(!bls_verify(&pk, &sig, &tampered));
        }

        /// For N keypairs each signing the SAME message,
        /// `verify(aggregate_pks, aggregate_sigs, msg)` accepts iff all
        /// individual verifies accept (which they always do here).
        #[test]
        fn bls_aggregate_equals_individual(
            msg in proptest::collection::vec(any::<u8>(), 0..64),
            seeds in proptest::collection::vec(arb_bls_seed(), 1..8),
        ) {
            let sks: Vec<_> = seeds
                .iter()
                .map(|s| BlsSecretKey::new(s).expect("valid ikm"))
                .collect();
            let pks: Vec<_> = sks.iter().map(BlsSecretKey::public_key).collect();
            let sigs: Vec<_> = sks.iter().map(|s| s.sign(&msg)).collect();

            // Each individual signature verifies.
            let mut all_individual = true;
            for (p, s) in pks.iter().zip(sigs.iter()) {
                all_individual &= bls_verify(p, s, &msg);
            }
            prop_assert!(all_individual);

            let pk_refs: Vec<_> = pks.iter().collect();
            let sig_refs: Vec<_> = sigs.iter().collect();
            let agg_pk = aggregate_public_keys(&pk_refs).expect("agg pk");
            let agg_sig = aggregate_signatures(&sig_refs).expect("agg sig");

            // aggregate(verify) accepts iff all individual verifies accept.
            prop_assert_eq!(bls_verify(&agg_pk, &agg_sig, &msg), all_individual);
        }

        /// `pubkey_bytes_to_address` is a deterministic pure function: the same
        /// public-key bytes always map to the same address.
        #[test]
        fn address_is_pure_fn_of_pubkey(seed in arb_secp_seed()) {
            let sk = PrivateKey::from_bytes(&seed).expect("valid scalar");
            let pk_bytes = sk.public_key().bytes();
            let a = pubkey_bytes_to_address(&pk_bytes);
            let b = pubkey_bytes_to_address(&pk_bytes);
            prop_assert_eq!(a, b);
        }
    }

    /// Build the high-S counterpart of a low-S `[r||s||v]` signature by replacing
    /// S with `order - S`. Returns `None` if the arithmetic would be degenerate
    /// (S == 0, which `sign_hash` never produces). The recovery byte is left
    /// untouched; `verify_format` rejects on the S check before recovery, which
    /// is exactly the malleability rule we are asserting.
    fn high_s_variant(sig: &[u8; SIGNATURE_LEN]) -> Option<[u8; SIGNATURE_LEN]> {
        // The secp256k1 group order N (big-endian).
        const N: [u8; 32] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
            0xd0, 0x36, 0x41, 0x41,
        ];
        let mut s = [0u8; 32];
        s.copy_from_slice(&sig[32..64]);
        if s.iter().all(|&b| b == 0) {
            return None;
        }
        // high_s = N - s (big-endian subtraction).
        let mut high_s = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let diff = i16::from(N[i]) - i16::from(s[i]) - borrow;
            if diff < 0 {
                high_s[i] = (diff + 256) as u8;
                borrow = 1;
            } else {
                high_s[i] = diff as u8;
                borrow = 0;
            }
        }
        let mut out = *sig;
        out[32..64].copy_from_slice(&high_s);
        Some(out)
    }
}
