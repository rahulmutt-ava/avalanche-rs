// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Quorum-certificate crypto round-trip tests (port of `qc_test.go`): build a
//! real BLS-aggregated QC, serialize/deserialize it through the canoto wire
//! format, and verify the aggregated signature over the chain/network-tagged
//! message.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use assert_matches::assert_matches;

use ava_crypto::bls::SecretKey;
use ava_simplex::Error;
use ava_simplex::messages::{BlsVerifier, aggregate, encode_message_to_sign, quorum};
use ava_types::id::Id;
use ava_types::node_id::NodeId;

const NETWORK_ID: u32 = 9999;

fn signer(seed: u8) -> (NodeId, SecretKey) {
    let sk = SecretKey::new(&[seed; 32]).expect("valid IKM");
    (NodeId::from([seed; 20]), sk)
}

/// Builds a verifier over `n` validators plus their secret keys.
fn membership(n: u8, chain_id: Id) -> (BlsVerifier, Vec<(NodeId, SecretKey)>) {
    let mut keys = Vec::new();
    let mut pairs = Vec::new();
    for i in 1..=n {
        let (node, sk) = signer(i);
        pairs.push((node, sk.public_key()));
        keys.push((node, sk));
    }
    (BlsVerifier::new(pairs, NETWORK_ID, chain_id), keys)
}

/// `qc_aggregate_verify_roundtrip` — a real aggregated QC over a quorum of
/// signers verifies, and its canoto bytes round-trip through deserialization.
#[test]
fn qc_aggregate_verify_roundtrip() {
    let chain_id = Id::from([7u8; 32]);
    let (verifier, keys) = membership(4, chain_id);
    let q = quorum(4);
    assert_eq!(q, 3);

    let msg = b"finalize-block-header";
    let to_sign = encode_message_to_sign(msg, chain_id, NETWORK_ID);

    // Each validator signs the chain/network-tagged message.
    let signatures: Vec<(NodeId, _)> = keys
        .iter()
        .map(|(node, sk)| (*node, sk.sign(&to_sign)))
        .collect();

    let qc = aggregate(&verifier, &signatures).expect("aggregate quorum");
    assert_eq!(qc.signers().len(), q);

    // Verifies against the original (untagged) message.
    qc.verify(msg).expect("QC verifies");

    // Wire round-trip: bytes -> QC -> verify.
    let bytes = qc.bytes();
    let parsed = ava_simplex::QC::from_bytes(&bytes, verifier.clone()).expect("parse QC");
    assert_eq!(parsed.signers().len(), q);
    parsed.verify(msg).expect("parsed QC verifies");

    // Tampering with the message breaks verification.
    assert_matches!(
        parsed.verify(b"different-message"),
        Err(Error::SignatureVerificationFailed)
    );
}

/// `qc_requires_quorum` — fewer than quorum signatures is rejected.
#[test]
fn qc_requires_quorum() {
    let chain_id = Id::from([7u8; 32]);
    let (verifier, keys) = membership(4, chain_id);

    let msg = b"x";
    let to_sign = encode_message_to_sign(msg, chain_id, NETWORK_ID);
    // Only 2 signatures, quorum is 3.
    let signatures: Vec<(NodeId, _)> = keys
        .iter()
        .take(2)
        .map(|(node, sk)| (*node, sk.sign(&to_sign)))
        .collect();

    let err = aggregate(&verifier, &signatures).err().expect("rejected");
    assert_matches!(
        err,
        Error::UnexpectedSigners {
            expected: 3,
            got: 2
        }
    );
}

/// `qc_rejects_unknown_signer` — a signer outside the membership set fails.
#[test]
fn qc_rejects_unknown_signer() {
    let chain_id = Id::from([7u8; 32]);
    let (verifier, keys) = membership(3, chain_id);

    let msg = b"y";
    let to_sign = encode_message_to_sign(msg, chain_id, NETWORK_ID);
    let mut signatures: Vec<(NodeId, _)> = keys
        .iter()
        .map(|(node, sk)| (*node, sk.sign(&to_sign)))
        .collect();
    // Replace one signer's node id with an outsider.
    let (_outsider_node, outsider_sk) = signer(200);
    signatures[0] = (NodeId::from([250u8; 20]), outsider_sk.sign(&to_sign));

    let err = aggregate(&verifier, &signatures).err().expect("rejected");
    assert_matches!(err, Error::SignerNotFound);
}
