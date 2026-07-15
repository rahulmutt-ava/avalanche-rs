// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Cancun syntactic header clamp (M9.15 task 8f — review Important-1).
//!
//! coreth's `wrappedBlock.syntacticVerify`
//! (`graft/coreth/plugin/evm/wrapped_block.go:493-518`) fail-closes, at Cancun
//! (== Etna on Avalanche), any block whose header does not carry **exactly**
//! `parentBeaconRoot == 0x0`, `blobGasUsed == 0`, `excessBlobGas == 0` — and
//! pre-Cancun any block where any of the three is present at all. Its
//! `core/block_validator.go:85-109` (`ValidateBody`) additionally counts the
//! blob hashes across the body's txs against `header.blobGasUsed /
//! BlobTxBlobGasPerBlob`, which — with the header clamped to 0 — excludes every
//! type-3 blob transaction syntactically.
//!
//! The rung-5 fix (6540888) enabled Cancun-active execution without porting
//! that clamp, leaving Rust fail-open where Go fail-closes: a proposer could
//! craft a self-consistent block (it controls the state root) with a nonzero
//! `excessBlobGas`/`blobGasUsed`/`parentBeaconRoot` or a blob tx that Go
//! rejects and Rust accepts — a consensus-split vector.
//!
//! These tests take the live-captured local-network block 1
//! (`vectors/cchain/block_wire/live_local_block1.json`, Cancun-active — the
//! local schedule activates Etna at genesis) as the base, mutate each clamped
//! field via decode → `into_parts` → re-assemble (the header hash is recomputed
//! from the mutated header, so the block stays self-consistent; the proposervm
//! container is bypassed by feeding the inner coreth bytes to `parse_block`,
//! exactly as `live_block_adopt.rs` does), and assert `verify` rejects each
//! with the coreth error.

use ava_evm::block::{AvaBlockParts, assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::AvaChainSpec;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{B256, Chain, U256};
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use tokio_util::sync::CancellationToken;

/// Extracts the inner coreth block bytes from a proposervm unsigned post-fork
/// container (same layout as `live_block_adopt.rs`).
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
/// does) and drives the full `parse_block → verify` pipeline on `bytes`,
/// returning the first error from EITHER stage as a string. A malformed block
/// (e.g. a blob tx the C-Chain wire codec cannot represent) is rejected at
/// `parse_block`; a well-formed-but-invalid block is rejected at `verify`. Both
/// are "the block is not adopted", so the pipeline error is what callers assert.
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
/// decode → parts → assemble path every mutant uses, still verifies. Proves
/// the mutants below fail because of the mutation, not the harness.
#[tokio::test]
async fn unmutated_reassembled_live_block_still_verifies() {
    let bytes = mutated_live_block(|_| {});
    parse_and_verify(&bytes)
        .await
        .expect("unmutated live block 1 verifies (harness regression)");
}

/// coreth `wrapped_block.go:505-506`: at Cancun `*ExcessBlobGas != 0` →
/// `errInvalidExcessBlobGas`. Execution alone cannot catch this (no tx reads
/// `BLOBBASEFEE` in the live block, so the attacker-declared state root still
/// matches) — pre-clamp Rust ACCEPTED this block.
#[tokio::test]
async fn nonzero_excess_blob_gas_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.excess_blob_gas = Some(1));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("nonzero excessBlobGas must fail syntactic verification");
    assert!(
        err.contains("invalid excessBlobGas"),
        "expected coreth errInvalidExcessBlobGas parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:501-502`: at Cancun `*BlobGasUsed != 0` →
/// `errBlobsNotEnabled` ("blobs not enabled on avalanche networks").
#[tokio::test]
async fn nonzero_blob_gas_used_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.blob_gas_used = Some(131_072));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("nonzero blobGasUsed must fail syntactic verification");
    assert!(
        err.contains("blobs not enabled"),
        "expected coreth errBlobsNotEnabled parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:497-498`: at Cancun `*ParentBeaconRoot !=
/// common.Hash{}` → `errParentBeaconRootNonEmpty`. The local genesis does not
/// deploy the EIP-4788 contract, so the syscall is a no-op and the state root
/// still matches — pre-clamp Rust ACCEPTED this block.
#[tokio::test]
async fn nonzero_parent_beacon_root_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.parent_beacon_root = Some(B256::repeat_byte(1)));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("nonzero parentBeaconRoot must fail syntactic verification");
    assert!(
        err.contains("parentBeaconRoot"),
        "expected coreth errParentBeaconRootNonEmpty parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:420-421` (ungated): a nonzero header `MixDigest`
/// is rejected on the C-Chain. Guarding it closes an adversarial PREVRANDAO
/// fail-open (a Byzantine block with a nonzero mix digest that Go rejects but
/// pre-guard Rust would execute). Honest Go blocks and Rust-built blocks both
/// carry mix == 0, so this never fires on the live follower arm.
#[tokio::test]
async fn nonzero_mix_digest_is_rejected() {
    let bytes = mutated_live_block(|p| p.header.mix_digest = B256::repeat_byte(1));
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("nonzero mixDigest must fail syntactic verification");
    assert!(
        err.contains("invalid mix digest"),
        "expected coreth invalid-mix-digest parity, got: {err}"
    );
}

/// coreth `wrapped_block.go:495-496`: at Cancun `ParentBeaconRoot == nil` →
/// `errMissingParentBeaconRoot`. Pre-clamp this was rejected only by the
/// EIP-4788 system call deep inside execution (an alloy-evm error, not the
/// coreth sentinel); the clamp rejects it syntactically with Go's message.
#[tokio::test]
async fn missing_parent_beacon_root_is_rejected_with_coreth_error() {
    let bytes = mutated_live_block(|p| p.header.parent_beacon_root = None);
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("missing parentBeaconRoot must fail verification");
    assert!(
        err.contains("missing parentBeaconRoot"),
        "expected coreth errMissingParentBeaconRoot parity, got: {err}"
    );
}

/// A type-3 blob tx in the block body must be rejected on the C-Chain, which
/// has no EIP-4844 blob support. coreth rejects it via layered checks — the
/// header clamp forces `blobGasUsed == 0` (`wrapped_block.go:499-502`) and
/// `ValidateBody` (`core/block_validator.go:100-104`) requires the body blob
/// count to match — but the C-Chain block **wire codec itself carries no blob
/// sidecar**, so a type-3 envelope fails to round-trip through
/// `decode_ava_evm_block`'s body decoder first (the earliest, and equally
/// valid, rejection layer: a blob tx can never appear in a well-formed C-Chain
/// block). This test pins the property that matters for consensus — the block
/// is rejected, never adopted — rather than the exact layer, since both the
/// decode rejection and the `blob gas used mismatch` clamp are correct coreth
/// parity outcomes for "no blobs on Avalanche".
#[tokio::test]
async fn blob_tx_in_body_is_rejected() {
    use ava_evm_reth::{EvmSignature, SignableTransaction, TransactionSigned, TxEip4844};

    let blob_tx = TxEip4844 {
        chain_id: 43112,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: 100_000_000_000,
        max_priority_fee_per_gas: 0,
        to: ava_evm_reth::Address::ZERO,
        value: U256::ZERO,
        access_list: Default::default(),
        blob_versioned_hashes: vec![B256::repeat_byte(1)],
        max_fee_per_blob_gas: 1,
        input: Default::default(),
    };
    let sig = EvmSignature::new(U256::from(1), U256::from(1), false);
    let signed = TransactionSigned::Eip4844(blob_tx.into_signed(sig));

    let bytes = mutated_live_block(|p| p.transactions.push(signed));
    // Rejected — the property under test. Either the body decoder rejects the
    // blob envelope or the clamp's blob-count parity fires; both are correct.
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("a type-3 blob tx in the body must be rejected on the C-Chain");
    // Clamp layer surfaces "blob gas used mismatch"; the wire-decode layer
    // (`parse_block`) surfaces the engine-erased "evm vm/block error" — both
    // mean the blob tx was rejected, never adopted.
    assert!(
        err.contains("blob gas used mismatch") || err.contains("evm vm/block error"),
        "expected a blob-count clamp or wire-decode rejection, got: {err}"
    );
}
