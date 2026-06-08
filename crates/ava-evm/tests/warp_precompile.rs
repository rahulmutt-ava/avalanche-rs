// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Warp precompile + predicate pass tests (M6.22, spec 10 §6.5/§8/§17.5, spec 20
//! §7, G4).
//!
//! These exercise the load-bearing, named-test target of M6.22:
//!
//! - the BLS-aggregate **predicate pass** ([`run_predicates`]) that runs *before*
//!   EVM execution, verifies each warp message against the source-subnet
//!   [`WarpSet`] at the proposervm-pinned P-Chain height, and stashes a
//!   `Vec<bool>` of results keyed by predicate index (spec 20 §7.2);
//! - the [`WarpPrecompile`] `getVerifiedWarpMessage(index)` selector reading the
//!   cached predicate result (spec 20 §7.1);
//! - `sendWarpMessage` emitting the `SendWarpMessage` log + returning the
//!   unsigned-message ID (spec 20 §7.1);
//! - gas costs matching BOTH the pre-Granite and Granite `GasConfig` tables
//!   (spec 20 §7.3);
//! - the `requirePrimaryNetworkSigners` subnet-substitution branch (spec 20 §7.2
//!   step 3);
//! - `getBlockchainID` returning the snow-context chain id.
//!
//! Golden vectors (selectors + event topic): `tests/vectors/cchain/warp/`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use ava_crypto::bls;
use ava_evm::precompile::registry::{
    AvaBlockCtx, PrecompileCtx, PredicateResults, StatefulPrecompile,
};
use ava_evm::precompile::warp::{
    GRANITE_GAS_CONFIG, PRE_GRANITE_GAS_CONFIG, PredicateContext, WARP_PRECOMPILE_ADDRESS,
    WarpPrecompile, predicate_to_chunks, run_predicates,
};
use ava_evm_reth::{Address, B256, InstructionResult};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::bits::Bits;
use ava_validators::error::Result as VsResult;
use ava_validators::state::{GetCurrentValidatorOutput, ValidatorState, WarpSet};
use ava_validators::validator::GetValidatorOutput;
use ava_warp::payload::AddressedCall;
use ava_warp::{BitSetSignature, Message, Signature, UnsignedMessage};

const NETWORK_ID: u32 = 12_345;

/// The chain id of the verifying C-Chain (snow ctx ChainID).
fn this_chain_id() -> Id {
    Id::from([0x0Cu8; 32])
}
/// The source chain id the warp message claims to originate from.
fn source_chain_id() -> Id {
    Id::from([0x5Au8; 32])
}
/// The subnet that owns `source_chain_id`.
fn source_subnet_id() -> Id {
    Id::from([0x57u8; 32])
}
/// The local C-Chain subnet id (used by the requirePrimaryNetworkSigners branch).
fn local_subnet_id() -> Id {
    Id::from([0x10u8; 32])
}

/// A warp set with BLS keys; returns the set + the secret keys so the test can
/// produce real aggregate signatures.
fn make_warp_set(weights: &[u64]) -> (WarpSet, Vec<bls::SecretKey>) {
    let mut validators = Vec::new();
    let mut sks = Vec::new();
    let mut total = 0u64;
    for (i, &w) in weights.iter().enumerate() {
        let sk = bls::SecretKey::new(&[u8::try_from(i).unwrap() + 1; 32]).expect("sk");
        let pk = sk.public_key();
        let mut node = [0u8; 20];
        node[0] = u8::try_from(i).unwrap() + 1;
        validators.push(GetValidatorOutput {
            node_id: NodeId::from(node),
            public_key: Some(pk),
            weight: w,
        });
        total += w;
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

/// Build a signed warp [`Message`] over an `AddressedCall(source_address,
/// payload)` for `source_chain`, signed by the validators whose canonical index
/// is in `signer_indices`.
fn signed_message(
    source_chain: Id,
    source_address: &[u8],
    payload: &[u8],
    sks: &[bls::SecretKey],
    signer_indices: &[usize],
) -> Message {
    let call = AddressedCall {
        source_address: source_address.to_vec(),
        payload: payload.to_vec(),
    };
    let payload_bytes = ava_warp::payload::WarpPayload::AddressedCall(call)
        .marshal_payload()
        .expect("payload");
    let unsigned = UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: source_chain,
        payload: payload_bytes,
    };
    let unsigned_bytes = unsigned.marshal().expect("marshal");

    let mut bits = Bits::new();
    let mut sigs = Vec::new();
    for &idx in signer_indices {
        bits.add(idx as u64);
        sigs.push(sks[idx].sign(&unsigned_bytes));
    }
    let sig_refs: Vec<&bls::Signature> = sigs.iter().collect();
    let agg = bls::aggregate_signatures(&sig_refs).expect("agg");
    Message {
        unsigned_message: unsigned,
        signature: Signature::BitSet(BitSetSignature {
            signers: bits.bytes(),
            signature: agg.compress(),
        }),
    }
}

/// A minimal in-memory [`ValidatorState`] for the predicate pass.
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
    ) -> VsResult<std::collections::BTreeMap<NodeId, GetValidatorOutput>> {
        Ok(std::collections::BTreeMap::new())
    }
    async fn get_current_validator_set(
        &self,
        _subnet: Id,
    ) -> VsResult<(
        std::collections::BTreeMap<Id, GetCurrentValidatorOutput>,
        u64,
    )> {
        Ok((std::collections::BTreeMap::new(), 0))
    }
    async fn get_warp_validator_sets(&self, _height: u64) -> VsResult<HashMap<Id, WarpSet>> {
        Ok(self.sets.clone())
    }
}

/// Decode an ABI `bytes32` return value at `word`.
fn abi_bytes32(output: &[u8], word: usize) -> [u8; 32] {
    let mut b = [0u8; 32];
    b.copy_from_slice(&output[word * 32..word * 32 + 32]);
    b
}

#[tokio::test]
async fn predicate_verifies_then_precompile_reads() {
    // ---- Set up a 4-validator source subnet with BLS keys. -----------------
    let (set, sks) = make_warp_set(&[25, 25, 25, 25]); // total 100
    let source_addr = [0xABu8; 20];
    let payload = b"hello-warp".to_vec();

    // A message signed by 3 of 4 (weight 75 >= 67% of 100) → valid.
    let valid_msg = signed_message(source_chain_id(), &source_addr, &payload, &sks, &[0, 1, 2]);
    // A message signed by 1 of 4 (weight 25 < 67) → predicate fails.
    let invalid_msg = signed_message(source_chain_id(), &source_addr, &payload, &sks, &[0]);

    let valid_bytes = valid_msg.marshal().expect("marshal valid");
    let invalid_bytes = invalid_msg.marshal().expect("marshal invalid");

    // ---- Predicate context from the proposervm block ctx. ------------------
    let mut subnet_of = HashMap::new();
    subnet_of.insert(source_chain_id(), source_subnet_id());
    let mut sets = HashMap::new();
    sets.insert(source_subnet_id(), set.clone());
    let state = MockState { subnet_of, sets };

    let pctx = PredicateContext {
        network_id: NETWORK_ID,
        this_chain_id: this_chain_id(),
        local_subnet_id: local_subnet_id(),
        pchain_height: 42,
        quorum_numerator: 0, // 0 => default 67
        require_primary_network_signers: false,
    };

    // ---- Predicate pass over two warp predicates (index 0 valid, 1 invalid).
    let predicates = vec![
        predicate_to_chunks(&valid_bytes),
        predicate_to_chunks(&invalid_bytes),
    ];
    let results = run_predicates(&state, &pctx, &predicates)
        .await
        .expect("run_predicates");
    assert_eq!(results, vec![true, false], "predicate 0 valid, 1 invalid");

    // ---- Build the precompile ctx the precompile reads. --------------------
    let mut pred_results = PredicateResults::default();
    pred_results.set_warp(0, predicates.clone(), results.clone());

    let warp = WarpPrecompile::new(this_chain_id(), NETWORK_ID, false);

    let make_ctx = || PrecompileCtx {
        caller: Address::from(source_addr),
        value: ava_evm_reth::U256::ZERO,
        predicates: Arc::new(pred_results.clone()),
        block: AvaBlockCtx {
            pchain_height: 42,
            timestamp: 1_000,
            current_tx_index: 0,
        },
    };

    // ---- getBlockchainID returns the snow-ctx chain id. --------------------
    let get_chain_id_sel = [0x42, 0x13, 0xcf, 0x78];
    let out = warp
        .run(&get_chain_id_sel, 1_000_000, &make_ctx())
        .expect("getBlockchainID");
    assert_eq!(out.result, InstructionResult::Return);
    assert_eq!(&abi_bytes32(&out.output, 0), this_chain_id().as_bytes());
    // pre-Granite gas: GetBlockchainID == 2.
    assert_eq!(
        out.gas.total_gas_spent(),
        PRE_GRANITE_GAS_CONFIG.get_blockchain_id
    );

    // ---- getVerifiedWarpMessage(0) reads the cached VALID predicate. -------
    let mut input = vec![0x6f, 0x82, 0x53, 0x50];
    input.extend_from_slice(&u32_word(0));
    let out = warp.run(&input, 5_000_000, &make_ctx()).expect("gvm 0");
    assert_eq!(out.result, InstructionResult::Return);
    let (msg_source_chain, msg_sender, msg_payload, valid) = decode_verified_message(&out.output);
    assert!(valid, "predicate 0 must read as valid");
    assert_eq!(&msg_source_chain, source_chain_id().as_bytes());
    assert_eq!(&msg_sender, &source_addr);
    assert_eq!(msg_payload, payload);

    // ---- getVerifiedWarpMessage(1) reads the cached INVALID predicate. -----
    let mut input = vec![0x6f, 0x82, 0x53, 0x50];
    input.extend_from_slice(&u32_word(1));
    let out = warp.run(&input, 5_000_000, &make_ctx()).expect("gvm 1");
    let (_, _, _, valid) = decode_verified_message(&out.output);
    assert!(!valid, "predicate 1 must read as invalid (failed verify)");

    // ---- sendWarpMessage emits a log + returns the unsigned-message ID. ----
    let send_payload = b"outbound".to_vec();
    let mut input = vec![0xee, 0x5b, 0x48, 0xeb];
    input.extend_from_slice(&abi_encode_bytes(&send_payload));
    let out = warp.run(&input, 10_000_000, &make_ctx()).expect("send");
    assert_eq!(out.result, InstructionResult::Return);

    let expected_call = AddressedCall {
        source_address: source_addr.to_vec(),
        payload: send_payload.clone(),
    };
    let expected_unsigned = UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: this_chain_id(),
        payload: ava_warp::payload::WarpPayload::AddressedCall(expected_call)
            .marshal_payload()
            .unwrap(),
    };
    let expected_id = expected_unsigned.id().unwrap();
    assert_eq!(&abi_bytes32(&out.output, 0), expected_id.as_bytes());

    let logs = warp.take_logs();
    assert_eq!(logs.len(), 1, "exactly one SendWarpMessage log");
    let log = &logs[0];
    assert_eq!(log.address, WARP_PRECOMPILE_ADDRESS);
    assert_eq!(log.topics.len(), 3);
    let event_topic = B256::from(hex_lit(
        "56600c567728a800c0aa927500f831cb451df66a7af570eb4df4dfbf4674887d",
    ));
    assert_eq!(log.topics[0], event_topic);
    assert_eq!(&log.topics[1].as_slice()[12..], &source_addr);
    assert_eq!(log.topics[2].as_slice(), expected_id.as_bytes());

    // ---- Gas tables: both pre-Granite and Granite. -------------------------
    let warp_granite = WarpPrecompile::new(this_chain_id(), NETWORK_ID, true);
    let out = warp_granite
        .run(&get_chain_id_sel, 1_000_000, &make_ctx())
        .expect("getBlockchainID granite");
    assert_eq!(
        out.gas.total_gas_spent(),
        GRANITE_GAS_CONFIG.get_blockchain_id
    );
    assert_eq!(GRANITE_GAS_CONFIG.get_blockchain_id, 200);
    assert_eq!(PRE_GRANITE_GAS_CONFIG.get_blockchain_id, 2);

    // ---- requirePrimaryNetworkSigners subnet-substitution branch. ----------
    let (local_set, local_sks) = make_warp_set(&[50, 50]);
    let prim_chain = Id::from([0x99u8; 32]);
    let prim_msg = signed_message(prim_chain, &source_addr, &payload, &local_sks, &[0, 1]);
    let prim_bytes = prim_msg.marshal().unwrap();

    let mut subnet_of = HashMap::new();
    subnet_of.insert(prim_chain, ava_types::constants::PRIMARY_NETWORK_ID);
    let mut sets = HashMap::new();
    // The set is registered under the LOCAL subnet id (substitution target).
    sets.insert(local_subnet_id(), local_set.clone());
    let prim_state = MockState { subnet_of, sets };

    let prim_ctx = PredicateContext {
        require_primary_network_signers: false,
        ..pctx
    };
    let r = run_predicates(&prim_state, &prim_ctx, &[predicate_to_chunks(&prim_bytes)])
        .await
        .expect("prim run");
    assert_eq!(
        r,
        vec![true],
        "primary-network source verifies against local subnet set"
    );

    // With require_primary_network_signers == true (and source chain is NOT the
    // P-chain), substitution does NOT happen → the primary set is required, absent
    // here → predicate fails.
    let prim_ctx_req = PredicateContext {
        require_primary_network_signers: true,
        ..pctx
    };
    let mut subnet_of = HashMap::new();
    subnet_of.insert(prim_chain, ava_types::constants::PRIMARY_NETWORK_ID);
    let mut sets = HashMap::new();
    sets.insert(local_subnet_id(), local_set.clone());
    let prim_state2 = MockState { subnet_of, sets };
    let r = run_predicates(
        &prim_state2,
        &prim_ctx_req,
        &[predicate_to_chunks(&prim_bytes)],
    )
    .await
    .expect("prim req run");
    assert_eq!(
        r,
        vec![false],
        "require_primary_network_signers blocks local-subnet substitution"
    );
}

// ---- ABI helpers (test side) ----------------------------------------------

fn u32_word(v: u32) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[28..32].copy_from_slice(&v.to_be_bytes());
    w
}

fn abi_encode_bytes(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u_word(32));
    out.extend_from_slice(&u_word(b.len() as u64));
    out.extend_from_slice(b);
    let pad = (32 - (b.len() % 32)) % 32;
    out.extend(std::iter::repeat_n(0u8, pad));
    out
}

fn u_word(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..32].copy_from_slice(&v.to_be_bytes());
    w
}

/// Decode the ABI-encoded `(WarpMessage, bool)` return of getVerifiedWarpMessage.
fn decode_verified_message(output: &[u8]) -> ([u8; 32], [u8; 20], Vec<u8>, bool) {
    let tuple_off = be_usize(&output[0..32]);
    let valid = output[32 + 31] == 1;
    let t = &output[tuple_off..];
    let mut chain = [0u8; 32];
    chain.copy_from_slice(&t[0..32]);
    let mut sender = [0u8; 20];
    sender.copy_from_slice(&t[64 - 20..64]);
    let payload_off = be_usize(&t[64..96]);
    let plen = be_usize(&t[payload_off..payload_off + 32]);
    let payload = t[payload_off + 32..payload_off + 32 + plen].to_vec();
    (chain, sender, payload, valid)
}

fn be_usize(w: &[u8]) -> usize {
    let mut v = 0usize;
    for &b in &w[24..32] {
        v = (v << 8) | b as usize;
    }
    v
}

fn hex_lit(s: &str) -> [u8; 32] {
    let bytes = hex::decode(s).unwrap();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// The committed warp golden vectors (`tests/vectors/cchain/warp/selectors.json`)
/// match the in-code address, selectors, event topic, and BOTH `GasConfig`
/// tables (spec 20 §7.1/§7.3). A drift in either side breaks this.
#[test]
fn warp_constants_match_golden() {
    let raw = include_str!("vectors/cchain/warp/selectors.json");
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();

    // Address.
    let addr = v["address"].as_str().unwrap();
    assert_eq!(
        format!("{:?}", WARP_PRECOMPILE_ADDRESS).to_lowercase(),
        addr.to_lowercase()
    );

    // Event topic.
    let topic = v["event"]["SendWarpMessage(address,bytes32,bytes)"]
        .as_str()
        .unwrap();
    assert_eq!(
        topic,
        "56600c567728a800c0aa927500f831cb451df66a7af570eb4df4dfbf4674887d"
    );

    // Gas tables.
    let pg = &v["gas"]["preGranite"];
    assert_eq!(
        pg["getBlockchainID"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.get_blockchain_id)
    );
    assert_eq!(
        pg["getVerifiedWarpMessageBase"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.get_verified_warp_message_base)
    );
    assert_eq!(
        pg["perWarpSigner"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.per_warp_signer)
    );
    assert_eq!(
        pg["perWarpMessageChunk"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.per_warp_message_chunk)
    );
    assert_eq!(
        pg["verifyPredicateBase"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.verify_predicate_base)
    );
    assert_eq!(
        pg["sendWarpMessageBase"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.send_warp_message_base)
    );
    assert_eq!(
        pg["perWarpMessageByte"].as_u64(),
        Some(PRE_GRANITE_GAS_CONFIG.per_warp_message_byte)
    );

    let g = &v["gas"]["granite"];
    assert_eq!(
        g["getBlockchainID"].as_u64(),
        Some(GRANITE_GAS_CONFIG.get_blockchain_id)
    );
    assert_eq!(
        g["getVerifiedWarpMessageBase"].as_u64(),
        Some(GRANITE_GAS_CONFIG.get_verified_warp_message_base)
    );
    assert_eq!(
        g["perWarpSigner"].as_u64(),
        Some(GRANITE_GAS_CONFIG.per_warp_signer)
    );
    assert_eq!(
        g["perWarpMessageChunk"].as_u64(),
        Some(GRANITE_GAS_CONFIG.per_warp_message_chunk)
    );
    assert_eq!(
        g["verifyPredicateBase"].as_u64(),
        Some(GRANITE_GAS_CONFIG.verify_predicate_base)
    );
}

/// `handlePrecompileAccept` records the `SendWarpMessage` logs the precompile
/// emitted into the warp backend on block accept (spec 20 §3.1).
#[test]
fn handle_precompile_accept_records_sent_messages() {
    use ava_evm::precompile::warp::{WarpBackend, handle_precompile_accept};

    let warp = WarpPrecompile::new(this_chain_id(), NETWORK_ID, false);
    let caller = [0xCDu8; 20];
    let payload = b"to-be-signed".to_vec();

    let mut input = vec![0xee, 0x5b, 0x48, 0xeb];
    input.extend_from_slice(&abi_encode_bytes(&payload));
    let ctx = PrecompileCtx {
        caller: Address::from(caller),
        value: ava_evm_reth::U256::ZERO,
        predicates: Arc::new(PredicateResults::default()),
        block: AvaBlockCtx::default(),
    };
    warp.run(&input, 10_000_000, &ctx).expect("send");

    let logs = warp.take_logs();
    let backend = WarpBackend::new();
    assert!(backend.is_empty());
    handle_precompile_accept(&backend, &logs).expect("accept");
    assert_eq!(backend.len(), 1, "one message recorded on accept");

    // The recorded id must equal the unsigned-message id the log carried.
    let expected_unsigned = UnsignedMessage {
        network_id: NETWORK_ID,
        source_chain_id: this_chain_id(),
        payload: ava_warp::payload::WarpPayload::AddressedCall(AddressedCall {
            source_address: caller.to_vec(),
            payload: payload.clone(),
        })
        .marshal_payload()
        .unwrap(),
    };
    assert!(backend.contains(&expected_unsigned.id().unwrap()));
}

/// `predicate_gas` charges base + per-chunk + per-signer over the active fork
/// table (spec 20 §7.3).
#[test]
fn predicate_gas_matches_table() {
    use ava_evm::precompile::warp::{num_signers, predicate_gas};

    let (_, sks) = make_warp_set(&[1, 1, 1]);
    let msg = signed_message(source_chain_id(), &[0xABu8; 20], b"x", &sks, &[0, 2]);
    let raw = msg.marshal().unwrap();
    let chunks = predicate_to_chunks(&raw);
    let num_chunks = (chunks.len() / 32) as u64;

    assert_eq!(num_signers(&msg), 2, "two signers set in the bit-set");

    let pre = predicate_gas(&chunks, false).unwrap();
    let expected_pre = PRE_GRANITE_GAS_CONFIG.verify_predicate_base
        + PRE_GRANITE_GAS_CONFIG.per_warp_message_chunk * num_chunks
        + PRE_GRANITE_GAS_CONFIG.per_warp_signer * 2;
    assert_eq!(pre, expected_pre);

    let gr = predicate_gas(&chunks, true).unwrap();
    let expected_gr = GRANITE_GAS_CONFIG.verify_predicate_base
        + GRANITE_GAS_CONFIG.per_warp_message_chunk * num_chunks
        + GRANITE_GAS_CONFIG.per_warp_signer * 2;
    assert_eq!(gr, expected_gr);
}

/// The predicate chunk encoding round-trips and rejects malformed encodings
/// (coreth `vms/evm/predicate`).
#[test]
fn predicate_chunks_roundtrip() {
    for raw in [
        vec![],
        vec![0u8; 1],
        vec![0xAB; 31],
        vec![0xCD; 32],
        vec![0xEF; 70],
    ] {
        let chunks = predicate_to_chunks(&raw);
        assert_eq!(chunks.len() % 32, 0, "chunked to a 32-byte multiple");
        assert_eq!(
            ava_evm::precompile::warp::predicate_from_chunks(&chunks),
            Some(raw.clone()),
            "round-trip for len {}",
            raw.len()
        );
    }
    // A non-multiple-of-32 input is rejected.
    assert_eq!(
        ava_evm::precompile::warp::predicate_from_chunks(&[0u8; 31]),
        None
    );
    // All-zeros (no delimiter) is rejected.
    assert_eq!(
        ava_evm::precompile::warp::predicate_from_chunks(&[0u8; 32]),
        None
    );
}
