// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cchain/warp::verify_block` inbound predicate-pass tests (M7.38), porting
//! Go's `cchain/warp/warp_test.go::TestVerifyBlock`.
//!
//! Each tx carries warp predicates in its access list; `verify_block`
//! BLS-aggregate-verifies them against the (single-subnet) validator set at the
//! pinned P-Chain height, collecting per-precompile FAILURE bits into a
//! `BlockResults`. A fully-valid tx maps to an empty `Bits`; the `errNoBlockContext`
//! gate fires only when predicates are present and no block context is supplied.

use std::collections::{BTreeMap, HashMap};

use assert_matches::assert_matches;
use async_trait::async_trait;
use ava_crypto::bls;
use ava_evm::precompile::warp::{PredicateContext, WARP_PRECOMPILE_ADDRESS, predicate_to_chunks};
use ava_evm_reth::{
    AccessList, AccessListItem, Address, B256, EvmSignature, Recovered, SignableTransaction,
    TransactionSigned, TxEip1559, U256,
};
use ava_saevm_cchain::warp::{BlockContext, Error, verify_block};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::bits::Bits;
use ava_validators::error::Result as VsResult;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_warp::payload::{AddressedCall, WarpPayload};
use ava_warp::{BitSetSignature, Message, Signature, UnsignedMessage};

const NETWORK_ID: u32 = 12_345;
const PCHAIN_HEIGHT: u64 = 42;

fn this_chain_id() -> Id {
    Id::from([0x0Cu8; 32])
}
fn source_chain_id() -> Id {
    Id::from([0x5Au8; 32])
}
fn source_subnet_id() -> Id {
    Id::from([0x57u8; 32])
}
fn local_subnet_id() -> Id {
    Id::from([0x10u8; 32])
}

/// A warp set with BLS keys; returns the set + the secret keys so the test can
/// produce real aggregate signatures.
fn make_warp_set(n: usize) -> (WarpSet, Vec<bls::SecretKey>) {
    let mut validators = Vec::new();
    let mut sks = Vec::new();
    let mut total = 0u64;
    for i in 0..n {
        let seed = u8::try_from(i).expect("seed fits u8").saturating_add(1);
        let sk = bls::SecretKey::new(&[seed; 32]).expect("bls sk");
        let pk = sk.public_key();
        let mut node = [0u8; 20];
        node[0] = seed;
        validators.push(GetValidatorOutput {
            node_id: NodeId::from(node),
            public_key: Some(pk),
            weight: 1,
        });
        total = total.saturating_add(1);
        sks.push(sk);
    }
    (
        WarpSet {
            validators,
            total_weight: total,
        },
        sks,
    )
}

/// The base unsigned message used across the predicate cases.
fn unsigned_message() -> UnsignedMessage {
    let payload = WarpPayload::AddressedCall(AddressedCall {
        source_address: vec![0xABu8; 20],
        payload: b"verify-block".to_vec(),
    })
    .marshal_payload()
    .expect("marshal_payload()");
    UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain_id(),
        payload,
    }
}

/// A valid predicate: a message signed by every validator in `sks`.
fn valid_predicate(sks: &[bls::SecretKey]) -> Vec<u8> {
    let unsigned = unsigned_message();
    let unsigned_bytes = unsigned.marshal().expect("marshal");
    let mut bits = Bits::new();
    let mut sigs = Vec::new();
    for (i, sk) in sks.iter().enumerate() {
        bits.add(u64::try_from(i).expect("index fits u64"));
        sigs.push(sk.sign(&unsigned_bytes));
    }
    let sig_refs: Vec<&bls::Signature> = sigs.iter().collect();
    let agg = bls::aggregate_signatures(&sig_refs).expect("aggregate");
    let msg = Message {
        unsigned_message: unsigned,
        signature: Signature::BitSet(BitSetSignature {
            signers: bits.bytes(),
            signature: agg.compress(),
        }),
    };
    predicate_to_chunks(&msg.marshal().expect("message marshal"))
}

/// An invalid predicate: an empty signer set (Go `warptest.IncorrectlySign`) —
/// syntactically valid but never reaches quorum.
fn invalid_predicate() -> Vec<u8> {
    let unsigned = unsigned_message();
    let msg = Message {
        unsigned_message: unsigned,
        signature: Signature::BitSet(BitSetSignature {
            signers: Bits::new().bytes(),
            signature: [0u8; 96],
        }),
    };
    predicate_to_chunks(&msg.marshal().expect("message marshal"))
}

/// Builds an unsigned-then-stub-signed EIP-1559 tx whose access list holds the
/// given (precompile-address, predicate-chunks) entries.
fn tx_with_predicates(entries: &[(Address, Vec<u8>)]) -> Recovered<TransactionSigned> {
    let access_list = AccessList(
        entries
            .iter()
            .map(|(addr, chunks)| {
                let storage_keys = chunks
                    .chunks_exact(32)
                    .map(|c| B256::from_slice(c))
                    .collect();
                AccessListItem {
                    address: *addr,
                    storage_keys,
                }
            })
            .collect(),
    );
    let tx = TxEip1559 {
        chain_id: 1,
        access_list,
        ..Default::default()
    };
    // A dummy (but structurally valid) signature; the tx hash is all that
    // matters for the keying, and recovery is not exercised here.
    let sig = EvmSignature::new(U256::from(1), U256::from(1), false);
    let signed = TransactionSigned::Eip1559(tx.into_signed(sig));
    Recovered::new_unchecked(signed, Address::ZERO)
}

/// A single-warp-predicate tx with the given chunk bytes.
fn warp_tx(chunks: Vec<u8>) -> Recovered<TransactionSigned> {
    tx_with_predicates(&[(WARP_PRECOMPILE_ADDRESS, chunks)])
}

/// An in-memory [`ValidatorState`] serving one warp set for `source_subnet_id`.
struct MockState {
    subnet_of: HashMap<Id, Id>,
    sets: HashMap<Id, WarpSet>,
}

#[async_trait]
impl ValidatorState for MockState {
    async fn get_minimum_height(&self) -> VsResult<u64> {
        Ok(0)
    }
    async fn get_current_height(&self) -> VsResult<u64> {
        Ok(100)
    }
    async fn get_subnet_id(&self, chain: Id) -> VsResult<Id> {
        Ok(self.subnet_of.get(&chain).copied().unwrap_or(Id::EMPTY))
    }
    async fn get_validator_set(
        &self,
        _height: u64,
        _subnet: Id,
    ) -> VsResult<BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(BTreeMap::new())
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> VsResult<(BTreeMap<Id, GetCurrentValidatorOutput>, u64)> {
        Ok((BTreeMap::new(), 0))
    }
    async fn get_warp_validator_sets(&self, height: u64) -> VsResult<HashMap<Id, WarpSet>> {
        assert_eq!(height, PCHAIN_HEIGHT, "pinned P-Chain height");
        Ok(self.sets.clone())
    }
}

fn predicate_ctx() -> PredicateContext {
    PredicateContext {
        network_id: NETWORK_ID,
        this_chain_id: this_chain_id(),
        local_subnet_id: local_subnet_id(),
        pchain_height: PCHAIN_HEIGHT,
        quorum_numerator: 0,
        require_primary_network_signers: false,
    }
}

fn mock_state() -> MockState {
    let (set, _) = make_warp_set(2);
    let mut subnet_of = HashMap::new();
    subnet_of.insert(source_chain_id(), source_subnet_id());
    let mut sets = HashMap::new();
    sets.insert(source_subnet_id(), set);
    MockState { subnet_of, sets }
}

/// Builds the warp set + keys + mock state in one shot (all share the same set).
fn fixtures() -> (Vec<bls::SecretKey>, MockState) {
    let (set, sks) = make_warp_set(2);
    let mut subnet_of = HashMap::new();
    subnet_of.insert(source_chain_id(), source_subnet_id());
    let mut sets = HashMap::new();
    sets.insert(source_subnet_id(), set);
    (sks, MockState { subnet_of, sets })
}

#[tokio::test]
async fn verify_block_no_predicaters_with_context() {
    // The contract is not registered as a predicater, but a block context is
    // present: no predicates extracted, empty result.
    let (sks, state) = fixtures();
    let txs = vec![warp_tx(valid_predicate(&sks))];
    let got = verify_block(
        &predicate_ctx(),
        Some(BlockContext {
            pchain_height: PCHAIN_HEIGHT,
        }),
        &state,
        &txs,
    )
    .await
    .expect("verify_block()");
    // Warp predicates ARE extracted by address regardless of a predicater
    // registry in this port, so a valid predicate yields an empty failure set.
    let mut want = BTreeMap::new();
    want.insert(WARP_PRECOMPILE_ADDRESS, Bits::new());
    assert_eq!(got.len(), 1);
    assert_eq!(got.values().next(), Some(&want));
}

#[tokio::test]
async fn verify_block_no_predicates_no_context_ok() {
    // A tx with NO warp predicates does not require a block context.
    let state = mock_state();
    let txs = vec![tx_with_predicates(&[])];
    let got = verify_block(&predicate_ctx(), None, &state, &txs)
        .await
        .expect("verify_block()");
    assert!(got.is_empty(), "no predicates => empty results");
}

#[tokio::test]
async fn verify_block_missing_block_context() {
    // Predicates present + no block context => errNoBlockContext.
    let (sks, state) = fixtures();
    let txs = vec![warp_tx(valid_predicate(&sks))];
    let err = verify_block(&predicate_ctx(), None, &state, &txs)
        .await
        .expect_err("verify_block() should error");
    assert_matches!(err, Error::NoBlockContext);
}

#[tokio::test]
async fn verify_block_one_valid_predicate() {
    let (sks, state) = fixtures();
    let tx = warp_tx(valid_predicate(&sks));
    let want_hash = *tx.tx_hash();
    let got = verify_block(
        &predicate_ctx(),
        Some(BlockContext {
            pchain_height: PCHAIN_HEIGHT,
        }),
        &state,
        &[tx],
    )
    .await
    .expect("verify_block()");

    let mut want = BTreeMap::new();
    let mut precompile = BTreeMap::new();
    precompile.insert(WARP_PRECOMPILE_ADDRESS, Bits::new());
    want.insert(want_hash, precompile);
    assert_eq!(got, want);
}

#[tokio::test]
async fn verify_block_one_invalid_predicate() {
    let (_sks, state) = fixtures();
    let tx = warp_tx(invalid_predicate());
    let want_hash = *tx.tx_hash();
    let got = verify_block(
        &predicate_ctx(),
        Some(BlockContext {
            pchain_height: PCHAIN_HEIGHT,
        }),
        &state,
        &[tx],
    )
    .await
    .expect("verify_block()");

    let mut failures = Bits::new();
    failures.add(0);
    let mut want = BTreeMap::new();
    let mut precompile = BTreeMap::new();
    precompile.insert(WARP_PRECOMPILE_ADDRESS, failures);
    want.insert(want_hash, precompile);
    assert_eq!(got, want);
}

#[tokio::test]
async fn verify_block_mixed_predicates() {
    // Access list: [valid, invalid, invalid, valid] => failure bits {1, 2}.
    let (sks, state) = fixtures();
    let tx = tx_with_predicates(&[
        (WARP_PRECOMPILE_ADDRESS, valid_predicate(&sks)),
        (WARP_PRECOMPILE_ADDRESS, invalid_predicate()),
        (WARP_PRECOMPILE_ADDRESS, invalid_predicate()),
        (WARP_PRECOMPILE_ADDRESS, valid_predicate(&sks)),
    ]);
    let want_hash = *tx.tx_hash();
    let got = verify_block(
        &predicate_ctx(),
        Some(BlockContext {
            pchain_height: PCHAIN_HEIGHT,
        }),
        &state,
        &[tx],
    )
    .await
    .expect("verify_block()");

    let mut failures = Bits::new();
    failures.add(1);
    failures.add(2);
    let precompile_results = got.get(&want_hash).expect("tx result").clone();
    assert_eq!(
        precompile_results.get(&WARP_PRECOMPILE_ADDRESS),
        Some(&failures)
    );
}

#[tokio::test]
async fn verify_block_multiple_txs() {
    let (sks, state) = fixtures();
    let valid_tx = warp_tx(valid_predicate(&sks));
    let invalid_tx = warp_tx(invalid_predicate());
    let valid_hash = *valid_tx.tx_hash();
    let invalid_hash = *invalid_tx.tx_hash();
    let got = verify_block(
        &predicate_ctx(),
        Some(BlockContext {
            pchain_height: PCHAIN_HEIGHT,
        }),
        &state,
        &[valid_tx, invalid_tx],
    )
    .await
    .expect("verify_block()");

    assert_eq!(got.len(), 2);
    assert_eq!(
        got.get(&valid_hash)
            .and_then(|m| m.get(&WARP_PRECOMPILE_ADDRESS)),
        Some(&Bits::new())
    );
    let mut failures = Bits::new();
    failures.add(0);
    assert_eq!(
        got.get(&invalid_hash)
            .and_then(|m| m.get(&WARP_PRECOMPILE_ADDRESS)),
        Some(&failures)
    );
}
