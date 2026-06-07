// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain Warp signing + verification tests (M4.22).
//!
//! Spec: `specs/08-platformvm-pchain.md` §8; `specs/20-warp-icm.md` §2, §2.1,
//! §4, §5.1, §6.1. Go reference:
//! `../avalanchego/vms/platformvm/warp/{unsigned_message,signature,signer}_test.go`.
//!
//! Two tests:
//!
//! * `golden::pchain_warp_message` — a hand-laid-out byte-exact assertion that an
//!   [`UnsignedMessage`] marshals to `0x0000` (codec version) + `network_id` +
//!   `source_chain_id` + `len(payload)` + `payload`, and that `id() ==
//!   sha256(bytes)` (specs 20 §2.1).
//! * `conformance::pchain_warp_sign_verify` — local-sign two validators' BLS
//!   keys over the message, aggregate into a [`BitSetSignature`], verify against
//!   a canonical [`WarpSet`] served by a fake [`ValidatorState`] (mirroring
//!   M4.21's `get_warp_validator_sets`) with the §6 quorum, then flip a bit and
//!   confirm verification fails.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;

use ava_crypto::bls::{self, SecretKey};
use ava_crypto::hashing;
use ava_platformvm::warp::verifier::WarpSetVerifier;
use ava_platformvm::warp::{BitSetSignature, Message, Signature, UnsignedMessage};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::bits::Bits;
use ava_validators::error::Result as VResult;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;

mod golden {
    use super::*;

    #[test]
    fn pchain_warp_message() {
        let network_id: u32 = 10; // constants::UNIT_TEST_ID
        let source_chain_id = Id::from([0x42; 32]);
        let payload = b"payload".to_vec();

        let msg = UnsignedMessage {
            network_id,
            source_chain_id,
            payload: payload.clone(),
        };

        // Build the expected wire bytes by hand (specs 20 §2.1):
        //   codec_version (0x0000) | network_id (u32 BE) |
        //   source_chain_id (32) | len(payload) (u32 BE) | payload
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0x00, 0x00]); // CODEC_VERSION
        expected.extend_from_slice(&network_id.to_be_bytes());
        expected.extend_from_slice(&[0x42; 32]);
        expected.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        expected.extend_from_slice(&payload);

        let got = msg.marshal().expect("marshal");
        assert_eq!(got, expected, "byte-exact UnsignedMessage marshal");

        // id == sha256(bytes), single-pass.
        let expected_id = Id::from(hashing::sha256(&got));
        assert_eq!(msg.id().expect("id"), expected_id);

        // Round-trips through the full Message envelope.
        let full = Message {
            unsigned_message: msg.clone(),
            signature: Signature::BitSet(BitSetSignature::default()),
        };
        let parsed = Message::parse(&full.marshal().expect("marshal message")).expect("parse");
        assert_eq!(parsed.unsigned_message, msg);
    }
}

mod conformance {
    use super::*;

    /// A fake [`ValidatorState`] that serves one subnet's [`WarpSet`] at any
    /// height, mirroring what M4.21's `PChainValidatorManager` returns.
    struct FakeState {
        subnet_id: Id,
        warp_set: WarpSet,
    }

    #[async_trait]
    impl ValidatorState for FakeState {
        async fn get_minimum_height(&self) -> VResult<u64> {
            Ok(0)
        }

        async fn get_current_height(&self) -> VResult<u64> {
            Ok(1)
        }

        async fn get_subnet_id(&self, _chain: Id) -> VResult<Id> {
            Ok(self.subnet_id)
        }

        async fn get_validator_set(
            &self,
            _height: u64,
            _subnet: Id,
        ) -> VResult<BTreeMap<NodeId, GetValidatorOutput>> {
            Ok(BTreeMap::new())
        }

        async fn get_current_validator_set(
            &self,
            _subnet: Id,
        ) -> VResult<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
            Ok((BTreeMap::new(), 1))
        }

        async fn get_warp_validator_sets(&self, _height: u64) -> VResult<HashMap<Id, WarpSet>> {
            let mut sets = HashMap::new();
            sets.insert(self.subnet_id, self.warp_set.clone());
            Ok(sets)
        }
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::new(&[seed; 32]).expect("secret key")
    }

    #[tokio::test]
    async fn pchain_warp_sign_verify() {
        let network_id: u32 = 10;
        let source_chain_id = Id::from([0x42; 32]);
        let subnet_id = Id::from([0x11; 32]);

        let sk0 = secret_key(1);
        let sk1 = secret_key(2);

        // Build the canonical warp set: dedup-by-key is a no-op here (distinct
        // keys), sorted by uncompressed public-key bytes, exactly as M4.21's
        // `flatten_validator_set` produces.
        let mut entries = vec![
            GetValidatorOutput {
                node_id: NodeId::EMPTY,
                public_key: Some(sk0.public_key()),
                weight: 50,
            },
            GetValidatorOutput {
                node_id: NodeId::EMPTY,
                public_key: Some(sk1.public_key()),
                weight: 50,
            },
        ];
        entries.sort_by(|a, b| {
            a.public_key
                .as_ref()
                .unwrap()
                .serialize()
                .cmp(&b.public_key.as_ref().unwrap().serialize())
        });
        let total_weight = 100;
        let warp_set = WarpSet {
            validators: entries.clone(),
            total_weight,
        };

        let state = FakeState {
            subnet_id,
            warp_set: warp_set.clone(),
        };

        // The message to sign.
        let unsigned = UnsignedMessage {
            network_id,
            source_chain_id,
            payload: b"hello warp".to_vec(),
        };
        let unsigned_bytes = unsigned.marshal().expect("marshal");

        // Map each canonical-index validator to the secret key it holds, then sign
        // with both. Bits {0, 1} ⇒ both signers.
        let key_for = |out: &GetValidatorOutput| -> SecretKey {
            let pk = out.public_key.as_ref().unwrap().serialize();
            if pk == sk0.public_key().serialize() {
                secret_key(1)
            } else {
                secret_key(2)
            }
        };
        let sig0 = key_for(&warp_set.validators[0]).sign(&unsigned_bytes);
        let sig1 = key_for(&warp_set.validators[1]).sign(&unsigned_bytes);
        let agg = bls::aggregate_signatures(&[&sig0, &sig1]).expect("aggregate");

        let mut signers = Bits::new();
        signers.add(0);
        signers.add(1);

        let message = Message {
            unsigned_message: unsigned.clone(),
            signature: Signature::BitSet(BitSetSignature {
                signers: signers.bytes(),
                signature: agg.compress(),
            }),
        };

        // Verify against the height-pinned warp set with the §6 67/100 quorum.
        let verifier = WarpSetVerifier::new(&state, network_id, /* p_chain_height */ 1);
        verifier.verify(&message).await.expect("verify ok");

        // Only one signer (weight 50/100 = 50% < 67%) ⇒ quorum is actually
        // checked: a valid single signature must still be rejected on weight.
        let mut one_signer = Bits::new();
        one_signer.add(0);
        let single_message = Message {
            unsigned_message: message.unsigned_message.clone(),
            signature: Signature::BitSet(BitSetSignature {
                signers: one_signer.bytes(),
                signature: key_for(&warp_set.validators[0])
                    .sign(&unsigned_bytes)
                    .compress(),
            }),
        };
        assert!(
            verifier.verify(&single_message).await.is_err(),
            "below-quorum signing weight must fail"
        );

        // Flip a bit in the aggregate signature ⇒ verification must fail.
        let mut bad_sig_bytes = agg.compress();
        bad_sig_bytes[10] ^= 0x01;
        let bad_message = Message {
            unsigned_message: unsigned,
            signature: Signature::BitSet(BitSetSignature {
                signers: signers.bytes(),
                // A flipped bit may not parse as a valid subgroup point (→
                // ParseSignature) or may parse but fail to verify (→
                // InvalidSignature); either way verification fails.
                signature: bad_sig_bytes,
            }),
        };
        assert!(
            verifier.verify(&bad_message).await.is_err(),
            "flipped-bit signature must fail verification"
        );
    }
}
