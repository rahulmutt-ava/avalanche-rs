// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end guard for the `verifyHeaderGasFields` port (coreth
//! `consensus/dummy/consensus.go:125-176`): the FULL `ChainVm` verify entry
//! (`EvmVm::parse_block` → `Block::verify`) must reject a block whose fee/gas
//! header fields disagree with the parent-derived recompute, even though its
//! state root is valid — the Byzantine-proposer fail-open this port closes.
//!
//! The harness mirrors `cancun_clamp.rs`: it decodes the live-captured
//! local-network block 1 (`vectors/cchain/block_wire/live_local_block1.json`,
//! parent == genesis, Cancun/Etna active from genesis), mutates a single
//! fee/gas header field via decode → `into_parts` → re-assemble (the header
//! hash is recomputed, so the block stays self-consistent), and drives the full
//! `parse_block → verify` pipeline.
//!
//! Why the mutated blocks stay self-consistent (valid state root) — which is
//! what makes these true fail-open vectors rather than root mismatches:
//!  - `base_fee`: the inner block carries a single **legacy** value-transfer tx
//!    (see the vector `_comment`). Avalanche credits the FULL effective gas
//!    price to the coinbase (never burns the base fee — see
//!    `evmconfig.rs::reward_beneficiary`), and a legacy tx's effective price is
//!    its fixed `gasPrice`, independent of the header `base_fee` (as long as
//!    `gasPrice >= base_fee`, which a +1 bump preserves). So bumping `base_fee`
//!    by 1 changes no balance and leaves the state root valid.
//!  - `gas_limit`: a block-level cap only; a 21000-gas tx executes identically
//!    under `gas_limit` and `gas_limit + 1`, so the state root is unchanged.
//!
//! Pre-port, `verify` had no fee/gas header equality checks, so both mutants
//! were ACCEPTED (the fail-open). Post-port, `verify_header_gas_fields` runs
//! right after `syntactic_verify` (before any execution work) and rejects each
//! with coreth's sentinel.

use ava_evm::block::{AvaBlockParts, assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{Chain, U256};
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

/// Extracts the inner coreth block bytes from a proposervm unsigned post-fork
/// container (same layout as `cancun_clamp.rs` / `live_block_adopt.rs`).
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

/// Boots the follower's `EvmVm` over the local genesis and drives the full
/// `parse_block → verify` pipeline on `bytes`, returning the first error from
/// EITHER stage as a string (same shape as `cancun_clamp.rs`). The parent of
/// block 1 is the seeded genesis, resolved through `Shared::parent_header`.
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

/// Harness regression: the UNMUTATED vector, round-tripped through the same
/// decode → parts → assemble path every mutant uses, still verifies. Proves the
/// mutants below fail because of the mutation, not the harness — and that the
/// honest block's fee/gas fields pass `verify_header_gas_fields` (builder and
/// verifier share the same fee functions).
#[tokio::test]
async fn unmutated_reassembled_live_block_still_verifies() {
    let bytes = mutated_live_block(|_| {});
    parse_and_verify(&bytes)
        .await
        .expect("unmutated live block 1 verifies (harness regression)");
}

/// coreth `consensus/dummy/consensus.go:136-144`: the header `BaseFee` must
/// equal the parent-derived recompute (`"expected base fee %d, found %d"`).
/// The mutated block's state root is still valid (legacy tx, full-fee-to-
/// coinbase — see the module doc), so pre-port `verify` ACCEPTED it: the
/// Byzantine-proposer fail-open this port closes.
#[tokio::test]
async fn wrong_base_fee_is_rejected_by_full_verify() {
    let bytes = mutated_live_block(|p| {
        let bumped = p.header.base_fee.unwrap_or_default() + U256::from(1);
        p.header.base_fee = Some(bumped);
    });
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("a header base_fee disagreeing with the parent recompute must be rejected");
    assert!(
        err.contains("expected base fee"),
        "expected coreth base-fee-mismatch parity, got: {err}"
    );
}

/// coreth `consensus/dummy/consensus.go:128-130` → `customheader.VerifyGasLimit`
/// (`gas_limit.go`): the header `GasLimit` must equal the parent-derived
/// recompute (`errInvalidGasLimit`, rendered "invalid gas limit ..."). A block
/// gas-limit cap does not affect the execution of the block's txs, so the
/// mutated block's state root is still valid — pre-port `verify` ACCEPTED it.
#[tokio::test]
async fn wrong_gas_limit_is_rejected_by_full_verify() {
    let bytes = mutated_live_block(|p| {
        p.header.gas_limit = p.header.gas_limit.saturating_add(1);
    });
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("a header gas_limit disagreeing with the parent recompute must be rejected");
    assert!(
        err.contains("invalid gas limit"),
        "expected coreth errInvalidGasLimit parity, got: {err}"
    );
}
