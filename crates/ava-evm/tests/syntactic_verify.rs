// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Remaining `wrappedBlock.syntacticVerify` structural checks (M9.15 task L1).
//!
//! coreth's `syntacticVerify` (`plugin/evm/wrapped_block.go:398-527`) enforces,
//! in order: block number fits uint64 (:412), [difficulty — Task 5], nonce == 0
//! (:418), mixDigest == 0 (:425, ported earlier), [VerifyExtra — Task 5], body
//! version == 0 (:434), header txsHash matches the body (:439), header
//! uncleHash matches the (structurally empty) body (:444/:453), coinbase ==
//! the blackhole address (:449), pre-AP1/pre-AP3 minimum tx gas price
//! (:458-473), BaseFee non-nil at AP3+ (:476-483), BlockGasCost non-nil at
//! AP4+ (:486-495), and the Cancun header clamp (:498-522, ported earlier).
//!
//! These tests mutate the live-captured local-network block 1 (Cancun-active
//! by construction — the local schedule activates Etna at genesis), so the
//! min-gas-price / nil-BaseFee / nil-BlockGasCost checks cannot be exercised
//! here (every fork is already active, and the header's optional-tail RLP
//! encoding cannot represent a "hole" in the middle of the optional fields);
//! those three live as unit tests in `block.rs`'s `#[cfg(test)]` module
//! instead, against a hand-built header + a spec whose schedule makes the
//! target phase active/inactive.
//!
//! No `oversized_block_number_is_rejected` mutation test exists here: unlike
//! coreth's `ethHeader.Number *big.Int`, [`ava_evm::block::AvaHeader::number`]
//! already decodes as a Rust `u64` (`block.rs::AvaHeader::decode_rlp`), so a
//! wire-level "block number too large for uint64" value cannot be constructed
//! at this layer — `Error::InvalidBlockNumber` exists (mirroring the coreth
//! sentinel / check order for Task 6) but its guard is unreachable given the
//! current header representation.

use ava_evm::block::{AvaBlockParts, assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{Address, B256, Bytes, Chain, U256};
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

/// Extracts the inner coreth block bytes from a proposervm unsigned post-fork
/// container (same layout as `live_block_adopt.rs` / `cancun_clamp.rs`).
fn inner_block_of(container: &[u8]) -> &[u8] {
    let cert_len = u32::from_be_bytes(container[54..58].try_into().expect("cert len"));
    assert_eq!(cert_len, 0, "unsigned post-fork block carries no cert");
    let block_len = u32::from_be_bytes(container[58..62].try_into().expect("block len")) as usize;
    &container[62..62 + block_len]
}

/// The local C-Chain spec (network 12345, chain 43112) the live vector was
/// produced under — Etna (== Cancun) active from genesis.
fn local_spec() -> AvaChainSpec {
    AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(43112))
}

/// Decodes the captured live block 1, applies `mutate` to its parts, and
/// re-assembles it into fresh self-consistent wire bytes (block ID recomputed
/// over the mutated header).
fn mutated_live_block(mutate: impl FnOnce(&mut AvaBlockParts)) -> Vec<u8> {
    let vector: serde_json::Value = serde_json::from_str(include_str!(
        "vectors/cchain/block_wire/live_local_block1.json"
    ))
    .expect("live_local_block1.json parses");
    let container = hex::decode(vector["container_hex"].as_str().expect("container_hex"))
        .expect("container hex decodes");
    let inner = inner_block_of(&container);
    let spec = local_spec();
    let block = decode_ava_evm_block(inner, &spec).expect("decode live inner block");
    let mut parts = block.into_parts();
    mutate(&mut parts);
    assemble_ava_block(parts, &spec)
        .expect("assemble mutated block")
        .encoded_bytes()
        .to_vec()
}

/// Boots the follower's `EvmVm` over the local genesis (as `live_block_adopt`
/// does) and drives the full `parse_block -> verify` pipeline on `bytes`,
/// returning the first error from EITHER stage as a string.
async fn parse_and_verify(bytes: &[u8]) -> Result<(), String> {
    let genesis_json = include_str!("vectors/cchain/genesis/local.json");
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the local genesis");
    let token = CancellationToken::new();
    let blk = vm
        .parse_block(&token, bytes)
        .await
        .map_err(|e| e.to_string())?;
    blk.verify(&token).await.map_err(|e| e.to_string())
}

/// Harness regression: every new check added by this task must still accept
/// the unmutated, Go-produced live block. Guards against over-strict ports —
/// the builder (Task 2) already satisfies every check exercised here.
#[tokio::test]
async fn unmutated_live_block_still_passes_new_checks() {
    let bytes = mutated_live_block(|_| ());
    parse_and_verify(&bytes)
        .await
        .expect("live Go block must verify");
}

/// coreth `wrapped_block.go:418`: `Nonce.Uint64() != 0` -> `errInvalidNonce`.
#[tokio::test]
async fn nonzero_nonce_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.nonce = [0, 0, 0, 0, 0, 0, 0, 1]);
    let err = parse_and_verify(&bytes).await.expect_err("nonzero nonce");
    assert!(
        err.contains("invalid nonce"),
        "coreth wrapped_block.go:418 parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:434`: a nonzero body extension version is invalid.
#[tokio::test]
async fn nonzero_body_version_is_rejected() {
    let bytes = mutated_live_block(|p| p.version = 7);
    let err = parse_and_verify(&bytes).await.expect_err("nonzero version");
    assert!(
        err.contains("invalid version"),
        "coreth wrapped_block.go:434 parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:439`: header `TxHash` must match the body's
/// derived transactions root.
#[tokio::test]
async fn wrong_tx_root_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.tx_root = B256::repeat_byte(0x11));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("tx root mismatch");
    assert!(
        err.contains("does not match calculated txs hash"),
        "coreth wrapped_block.go:439 parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:444`: header `UncleHash` must match the
/// (structurally empty) body's derived uncle hash.
#[tokio::test]
async fn wrong_uncle_hash_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.uncle_hash = B256::repeat_byte(0x22));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("uncle hash mismatch");
    assert!(
        err.contains("invalid uncle hash"),
        "coreth wrapped_block.go:444 parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:449`: the C-Chain coinbase must be the blackhole
/// address.
#[tokio::test]
async fn non_blackhole_coinbase_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.coinbase = Address::repeat_byte(0x33));
    let err = parse_and_verify(&bytes).await.expect_err("bad coinbase");
    assert!(
        err.contains("invalid coinbase"),
        "coreth wrapped_block.go:449 parity, got: {err}"
    );
}

// NOTE: coreth `wrapped_block.go:452-455` (`errUnclesUnsupported`) rejects a
// non-empty uncle list. In Rust this now fires at wire decode
// (`decode_ava_evm_block` -> `decode_uncle_list`, block.rs) rather than in
// `syntactic_verify` — the block wire codec itself admits only an empty uncle
// list once `decode_uncle_list` is fixed to reject a non-empty one (the
// Go-parity fix this task makes; see block.rs's `decode_uncle_list` doc
// comment). `AvaBlockParts` has no field to carry a non-empty uncle list
// (uncles are always encoded as `[]` by `assemble_ava_block`, matching the
// C-Chain's structural guarantee), so this is exercised as a direct unit test
// of `decode_uncle_list` in `block.rs`'s `#[cfg(test)]` module instead of
// here — adding a synthetic non-empty-uncle-list wire fixture would require
// hand-rolling raw RLP bytes divorced from any real block shape.

/// Regression guard for the `unmutated_reassembled_live_block_still_verifies`
/// sibling test in `cancun_clamp.rs` — kept here too since this file exercises
/// a disjoint set of mutations and should not depend on that file (test files
/// repeat helpers rather than share them, per the repo convention).
#[tokio::test]
async fn unmutated_reassembled_live_block_still_verifies() {
    let bytes = mutated_live_block(|_| {});
    parse_and_verify(&bytes)
        .await
        .expect("unmutated live block 1 verifies (harness regression)");
}

/// coreth `wrapped_block.go:415`: `Difficulty.Cmp(common.Big1) != 0` ->
/// `errInvalidDifficulty` (`consensus.go:233-235`'s `Prepare` stamps every
/// built header's difficulty to exactly 1; anything else is invalid).
#[tokio::test]
async fn zero_difficulty_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.difficulty = U256::ZERO);
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("difficulty must be 1");
    assert!(
        err.contains("invalid difficulty"),
        "coreth wrapped_block.go:415 parity, got: {err}"
    );
}

/// coreth `customheader/extra.go:122-130` (`VerifyExtra`, Fortuna arm): the
/// live local-network block 1 is Fortuna-active (the local schedule activates
/// every phase but Helicon at genesis), so a 3-byte extra — well short of the
/// 24-byte `acp176.StateSize` floor — is rejected here.
#[tokio::test]
async fn truncated_extra_is_rejected_at_fortuna() {
    let bytes = mutated_live_block(|p| p.header.extra = Bytes::from(vec![0u8; 3]));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("extra shorter than acp176 state");
    assert!(
        err.contains("invalid header.Extra length"),
        "coreth extra.go:122-130 parity, got: {err}"
    );
}
