// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! End-to-end guard for the semantic-verify family port (coreth
//! `wrapped_block.go:335-391` — `VerifyMinDelayExcess` + `VerifyTime`): the
//! full `parse_block → verify` entry must reject a block whose time /
//! min-delay-excess fields disagree with the rules, even though its state
//! root (and, for these mutants, every fee/gas field expectation) is valid.
//!
//! Restamp-free mutants: block 1's elapsed-time-from-genesis is ~5.6 years,
//! so `fee_state_before_block`'s advance saturates capacity at MaxCapacity
//! regardless of ±seconds-scale (or even +decades) shifts to the header time
//! fields — base_fee / extra-prefix / gas-limit / block-gas-cost recomputes
//! are unchanged and `verify_header_gas_fields` still passes, isolating the
//! NEW checks' sentinels.
//!
//! **`strip_time_milliseconds_is_rejected` note (deviation from the literal
//! task-brief mutation):** the header's optional RLP tail is positional —
//! `min_delay_excess` (t8) being present forces `time_milliseconds`'s (t7)
//! slot to also be *encoded*, and a `None` forced into that slot writes the
//! same single byte (`0x80`) that a genuine `Some(0)` would. This is
//! byte-for-byte the same ambiguity go-ethereum's own `rlp:"optional"`
//! (non-`nil`-tagged) pointer decoder has — `makeSimplePtrDecoder`
//! (`rlp/decode.go`) decodes an empty-string input as a *non-nil* pointer to
//! the zero value, never as a nil pointer, whenever any later optional field
//! is present. So "claim `min_delay_excess` while genuinely omitting
//! `time_milliseconds`" is not a constructible wire shape in Go OR here: the
//! mutation decodes back as `time_milliseconds = Some(0)`, not `None`. And
//! since `VerifyMinDelayExcess` runs before `VerifyTime` in Go's own order
//! (`wrapped_block.go:345,359`) and unconditionally requires
//! `min_delay_excess` at Granite, `ErrTimeMillisecondsRequired` is
//! structurally unreachable from the full block-verify path when
//! `min_delay_excess` is present — it can only fire via a direct call to
//! `verify_time` (already covered by
//! `feerules::semantic_verify_tests::verify_time_requires_time_milliseconds_at_granite`,
//! Task 1). The mutation below instead exercises the REACHABLE consequence:
//! the forced `Some(0)` reads as a wildly-earlier timestamp than the parent's,
//! which is caught (fail-closed, not accepted) by the already-landed
//! `verify_header_gas_fields`'s `fee_state_before_block` monotonic-time guard,
//! one stage before this task's new checks even run. The security property
//! (no fail-open) holds either way; only the sentinel differs from the
//! brief's literal expectation.

use ava_evm::block::{AvaBlockParts, assemble_ava_block, decode_ava_evm_block};
use ava_evm::chainspec::{AvaChainSpec, CChainGenesis};
use ava_evm::vm::EvmVm;
use ava_evm_reth::{B256, Chain};
use ava_snow::EngineState;
use ava_types::constants::LOCAL_ID;
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;
use tokio_util::sync::CancellationToken;

/// Extracts the inner coreth block bytes from a proposervm unsigned post-fork
/// container (same layout as `verify_gas_fields.rs` / `cancun_clamp.rs`).
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

/// The genesis header block 1's mutants are built against — same mechanism
/// `feerules.rs::fee_state_after_block_matches_live_go_block_extra` uses (the
/// genesis state root is irrelevant to the ACP-176 fee-state recompute, only
/// its time/number matter — `fee_state_after_block` seeds the zero state for a
/// number-0 parent).
fn genesis_header_of(spec: &AvaChainSpec) -> ava_evm::block::AvaHeader {
    let genesis = CChainGenesis::parse(include_str!("vectors/cchain/genesis/local.json"))
        .expect("parse local genesis");
    genesis.genesis_header(B256::ZERO, spec.network_upgrades())
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
/// EITHER stage as a string (same shape as `verify_gas_fields.rs`). The parent
/// of block 1 is the seeded genesis, resolved through `Shared::parent_header`.
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

/// As [`parse_and_verify`], but flips the VM to `NormalOp` (Go
/// `vm.bootstrapped.Set(true)`) before verifying, so `verify_intrinsic_gas`
/// (wrapped_block.go:372-379) actually runs. `EvmVm::set_state` stores this on
/// an atomic the block's `verify` reads live, so the ordering relative to
/// `parse_block` doesn't matter — only that it happens before `blk.verify`.
async fn parse_and_verify_bootstrapped(bytes: &[u8]) -> Result<(), String> {
    let genesis_json = include_str!("vectors/cchain/genesis/local.json");
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the local genesis");
    let token = CancellationToken::new();
    vm.set_state(&token, EngineState::NormalOp)
        .await
        .expect("set_state(NormalOp)");
    let blk = vm
        .parse_block(&token, bytes)
        .await
        .map_err(|e| e.to_string())?;
    blk.verify(&token).await.map_err(|e| e.to_string())
}

/// Harness regression: the UNMUTATED vector, round-tripped through the same
/// decode → parts → assemble path every mutant uses, still verifies — pins
/// that the NEW `VerifyMinDelayExcess`/`VerifyTime` checks don't false-reject
/// the honest arm.
#[tokio::test]
async fn honest_block_still_verifies() {
    let bytes = mutated_live_block(|_| {}); // identity re-assemble
    parse_and_verify(&bytes)
        .await
        .expect("honest live block verifies");
}

#[tokio::test]
async fn strip_time_milliseconds_is_rejected() {
    // See the module doc's "strip_time_milliseconds_is_rejected note": the
    // header also carries min_delay_excess (t8), which forces this mutation's
    // `None` to round-trip as `Some(0)` (the wire cannot distinguish "forced
    // placeholder for an absent field" from "explicit zero" — go-ethereum's
    // own rlp decoder has the identical ambiguity). The resulting header
    // reads as timestamped at the Unix epoch, which the already-landed
    // `verify_header_gas_fields` (its `fee_state_before_block` monotonic-time
    // guard) rejects BEFORE this task's new checks run — still a fail-closed
    // rejection, just not `VerifyTime`'s own Granite-required sentinel (that
    // branch is exercised directly by
    // `feerules::semantic_verify_tests::verify_time_requires_time_milliseconds_at_granite`).
    let bytes = mutated_live_block(|parts| {
        parts.header.time_milliseconds = None;
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("invalid fee state"),
        "want the fee-state monotonic-time guard to catch the forced Some(0) \
         timestamp, got: {err}"
    );
}

#[tokio::test]
async fn mismatched_time_milliseconds_is_rejected() {
    // VerifyTime time.go:94-101 — Time != TimeMilliseconds/1000.
    let bytes = mutated_live_block(|parts| {
        let ms = parts.header.time_milliseconds.expect("live block has ms");
        parts.header.time_milliseconds = Some(ms + 5_000); // +5s in ms only
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("TimeMilliseconds does not match header.Time"),
        "want ErrTimeMillisecondsMismatched, got: {err}"
    );
}

#[tokio::test]
async fn far_future_block_is_rejected() {
    // VerifyTime time.go:72-79 — beyond now+10s (prod path reads RealClock;
    // year-4000 is deterministically far-future for any test run).
    let bytes = mutated_live_block(|parts| {
        let t = 64_060_588_800u64; // 4000-01-01
        parts.header.time = t;
        parts.header.time_milliseconds = Some(t * 1000);
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("too far in the future"),
        "want ErrBlockTooFarInFuture, got: {err}"
    );
}

#[tokio::test]
async fn wrong_min_delay_excess_is_rejected() {
    // VerifyMinDelayExcess min_delay_excess.go:73-79 — an unreachable claim.
    // min_delay_excess is a bare header-tail field, not a fee-prefix input,
    // so no other expectation shifts.
    let bytes = mutated_live_block(|parts| {
        parts.header.min_delay_excess = Some(u64::MAX);
    });
    let err = parse_and_verify(&bytes).await.expect_err("must reject");
    assert!(
        err.contains("incorrect min delay excess"),
        "want errIncorrectMinDelayExcess, got: {err}"
    );
}

/// Lowers the claimed `gas_used` below the tx's intrinsic floor (21_000 for
/// the single legacy transfer) and restamps the ACP-176 extra prefix so the
/// mutated header stays self-consistent for every OTHER check (the exact
/// shape a Byzantine proposer would use to reach `verifyIntrinsicGas`).
fn understated_gas_used_block() -> Vec<u8> {
    let spec = local_spec();
    mutated_live_block(|parts| {
        parts.header.gas_used = 0;
        // Restamp the ACP-176 prefix: recompute the post-block fee state at
        // the mutated gas_used and splice it over the first STATE_SIZE bytes
        // of `extra` (the Durango predicate-results suffix is preserved).
        let genesis = genesis_header_of(&spec); // the same parent the honest
        // vector was built on.
        let after = ava_evm::feerules::fee_state_after_block(
            &spec,
            &genesis,
            parts.header.time,
            parts.header.time_milliseconds,
            0, // the mutated gas_used
            0, // no atomic gas in this block
            None,
        )
        .expect("restamp fee state");
        let mut extra = after.to_bytes().to_vec();
        extra.extend_from_slice(&parts.header.extra[ava_evm::feerules::acp176::STATE_SIZE..]);
        parts.header.extra = extra.into();
    })
}

#[tokio::test]
async fn understated_gas_used_is_rejected_when_bootstrapped() {
    // wrapped_block.go:321-329 — Σ intrinsic gas (21_000 for the single
    // legacy transfer) > claimed gas_used (0). Gated on bootstrapped, so the
    // driver must SetState(NormalOp) first. The extra prefix is restamped for
    // the new gas_used (fee_state_after_block consumes it); base_fee /
    // block_gas_cost / gas_limit are gas_used-independent recomputes.
    let bytes = understated_gas_used_block();
    let err = parse_and_verify_bootstrapped(&bytes)
        .await
        .expect_err("must reject");
    assert!(
        err.contains("intrinsic gas"),
        "want errTotalIntrinsicGasCostExceedsClaimed, got: {err}"
    );
}

#[tokio::test]
async fn understated_gas_used_skipped_while_bootstrapping() {
    // wrapped_block.go:376 — the SAME bytes verify while NOT bootstrapped
    // (the gate must skip, not reject) … and then fail the execution-layer
    // gas_used equality instead, which is the pre-existing backstop. Assert
    // the error does NOT name intrinsic gas.
    let bytes = understated_gas_used_block();
    let err = parse_and_verify(&bytes)
        .await
        .expect_err("execution backstop");
    assert!(
        !err.contains("intrinsic gas"),
        "bootstrapping node must skip verifyIntrinsicGas, got: {err}"
    );
}
