// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M9.15 Task 6 — the offline Go-oracle verdict leg (recorded differential):
//! REAL coreth code judges Rust-**built** C-Chain block bytes.
//!
//! This is a two-step recorded oracle (specs/02 §11.1), the same shape as the
//! M7.29 recovery differential (`tests/differential/go-oracle/
//! recovery_vector_emitter_test.go` + `tests/vectors/saevm/recovery_differential/`
//! + `sae_recovery.rs`), but here the emitter runs on the **Rust** side and the
//!   judge on the **Go** side (the M7.29 shape is the reverse):
//!
//! 1. [`emit_proposer_candidates`] (env-gated, operator step): builds the
//!    "honest" candidate — a real block the Task 2-5 builder produces on the
//!    committed `local.json` C-Chain genesis (`vectors/cchain/genesis/local.json`,
//!    already the Go-oracle genesis fixture `cancun_clamp.rs` boots — LOCAL_ID's
//!    schedule activates every fork through Granite at `InitiallyActiveTime`,
//!    matching Go's `upgradetest.Granite`), carrying one signed EVM tx — plus
//!    sixteen adversarial mutations of it (Task 6 added five `verifyHeaderGasFields`
//!    legs; Task 8 added six `semanticVerify`-family legs). Most are the
//!    decode → mutate → re-encode `cancun_clamp.rs:57-96` pattern; the restamp
//!    mutants recompute the ACP-176 prefix, and `trailing_sae_tail_field` is
//!    raw-byte surgery. Writes `<name>.rlp.hex` + a copy of the genesis JSON
//!    into the output directory.
//! 2. The companion Go judge (`tests/differential/go-oracle/
//!    rust_built_block_verdict_test.go`, dropped into `~/avalanchego/graft/
//!    coreth/plugin/evm/` to run) boots a real coreth test VM over the SAME
//!    genesis JSON, `ParseBlock`s + `Verify`s each candidate, and writes
//!    `verdicts.json`.
//! 3. [`proposer_verdicts_hold`] (per-PR): loads the committed `verdicts.json`,
//!    asserts the honest verdict is `accepted == true`, and for every
//!    adversarial candidate asserts BOTH the recorded Go verdict is a rejection
//!    whose error names the expected sentinel AND that Rust's own
//!    `EvmVm::parse_block` → `Block::verify` entry (the same one the `ChainVm`
//!    adapter drives) rejects the identical bytes with the matching Rust
//!    sentinel — i.e. Go and Rust reject the SAME candidate for the SAME
//!    reason.
//!
//! # Re-recording (operator, live mode)
//!
//! ```sh
//! ./scripts/check_oracle_binary.sh   # must print OK before recording
//! EMIT_PROPOSER_CANDIDATES=$PWD/crates/ava-evm/tests/vectors/proposer_verdict \
//!   cargo test -p ava-evm --test proposer_candidates -- --exact emit_proposer_candidates
//! cp tests/differential/go-oracle/rust_built_block_verdict_test.go \
//!    ~/avalanchego/graft/coreth/plugin/evm/
//! cd ~/avalanchego && AVALANCHEGO_COMMIT=$(git rev-parse HEAD) \
//! RUST_BLOCK_VERDICT_DIR=$OLDPWD/crates/ava-evm/tests/vectors/proposer_verdict \
//!   go test -run TestRustBuiltBlockVerdicts ./graft/coreth/plugin/evm/ && \
//!   rm graft/coreth/plugin/evm/rust_built_block_verdict_test.go
//! ```
//!
//! Without `EMIT_PROPOSER_CANDIDATES` set, [`emit_proposer_candidates`] is a
//! no-op, so the emitter never runs during a normal `cargo test`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use ava_crypto::secp256k1::PrivateKey;
use ava_database::{DynDatabase, MemDb};
use ava_evm::atomic::mempool::AtomicMempool;
use ava_evm::block::{
    AvaBlockParts, AvaHeader, EvmBlockContext, assemble_ava_block, decode_ava_evm_block,
};
use ava_evm::builder::BlockBuilderDriver;
use ava_evm::canonical::CanonicalStore;
use ava_evm::chainspec::{AvaChainSpec, CChainGenesis};
use ava_evm::evmconfig::{AvaEvmConfig, AvaNextBlockCtx};
use ava_evm::feerules::parent_fee_state_of;
use ava_evm::state::FirewoodStateProvider;
use ava_evm::vm::EvmVm;
use ava_evm_reth::{
    Address, B256, Chain, EvmSignature, RlpEncodable as _, RlpListHeader, SignableTransaction,
    SignerRecoverable, TransactionSigned, TxKind, TxLegacy, U256,
};
use ava_snow::EngineState;
use ava_types::constants::LOCAL_ID;
use ava_types::id::Id;
use ava_vm::block::ChainVm;
use ava_vm::vm::Vm;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// The well-known "ewoq" pre-funded private key on `local` networks (the same
/// constant `tests/differential/src/livenet.rs::EWOQ_KEY_HEX` carries).
///
/// Address: `0x8db97C7cEcE249c2b98bDC0226Cc4C2A57BF52FC` — the sole `alloc`
/// entry in `vectors/cchain/genesis/local.json`.
const EWOQ_KEY_HEX: &str = "56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027";

/// A gas price comfortably above the AP3 genesis base fee (225 gwei) so the
/// honest candidate's tx is never underpriced.
const HONEST_TX_GAS_PRICE: u128 = 300_000_000_000;

/// The committed C-Chain local genesis (also `cancun_clamp.rs`'s Go-oracle
/// fixture) — reused verbatim so both the Rust builder and the Go judge boot
/// from byte-identical genesis JSON.
fn local_genesis_json() -> &'static str {
    include_str!("vectors/cchain/genesis/local.json")
}

/// The committed corpus directory (workspace-rooted).
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/proposer_verdict")
}

/// A named header-corrupting mutation applied to the honest candidate's
/// decoded parts.
type Mutation = (&'static str, fn(&mut AvaBlockParts));

/// The five structural adversarial mutations (brief-mandated set): each takes
/// the honest candidate's decoded parts and corrupts exactly one
/// `syntacticVerify` check, leaving every earlier-checked field untouched so
/// Go's (and Rust's) first rejection is the intended one
/// (`wrapped_block.go:398-527` / `block.rs` `syntactic_verify` — both walk the
/// checks in the same order).
///
/// Task 6 adds five more, targeting the `verifyHeaderGasFields` legs ported in
/// Tasks 1-5 (coreth `consensus/dummy/consensus.go:125-176`): each corrupts
/// exactly one fee/gas equality check, leaving every earlier-checked field
/// untouched so the first rejection is the intended one (Go's check order:
/// GasLimit -> ExtraPrefix -> BaseFee -> BlockGasCost -> ExtData).
///
/// Task 8 adds two more `fn`-based mutants targeting the `semanticVerify`
/// family ported in the C-Chain semantic-verify branch (coreth
/// `wrapped_block.go:335-391` — `VerifyMinDelayExcess` / `VerifyTime`):
/// `missing_time_milliseconds` and `wrong_min_delay_excess`. Neither shifts the
/// header's timestamp, so neither disturbs the ACP-176 fee-state recompute —
/// they need no restamp and remain plain `AvaBlockParts` edits.
///
/// The other four Task-8 mutants CANNOT be plain edits and are emitted directly
/// by [`emit_proposer_candidates`]:
///
/// * `mismatched_time_milliseconds` / `far_future_time` — shifting the time
///   fields DOES change the fee-state recompute (contrary to the brief's
///   "restamp-free" prediction: the genesis fee state's `excess` is at its
///   attractor `0`, but its `capacity` is NOT saturated at block 1 — it grows
///   with elapsed time, so a later timestamp recomputes a larger capacity than
///   the honest `extra` prefix encodes). Because Rust runs
///   `verify_header_gas_fields` BEFORE `verify_time` (`block.rs:988,1002`),
///   without a restamp Rust would reject these at `IncorrectFeeState` before
///   ever reaching `VerifyTime`, whereas Go — which runs `VerifyTime` inside
///   `semanticVerify`, BEFORE its `verifyHeaderGasFields` — reaches the time
///   check. So these two are RESTAMPED (extra prefix recomputed at the mutated
///   time) to stay self-consistent for every other check and isolate
///   `VerifyTime` on BOTH sides — the same Byzantine-proposer shape
///   `understated_gas_used` uses.
/// * `understated_gas_used` (restamp) and `trailing_sae_tail_field` (raw-byte
///   splice) — as before.
const MUTATIONS: [Mutation; 12] = [
    ("zero_difficulty", |p| p.header.difficulty = U256::ZERO),
    ("missing_cancun_tail", |p| {
        p.header.parent_beacon_root = None;
        p.header.blob_gas_used = None;
        p.header.excess_blob_gas = None;
    }),
    ("wrong_tx_root", |p| {
        p.header.tx_root = B256::repeat_byte(0x11)
    }),
    ("bad_coinbase", |p| {
        p.header.coinbase = Address::repeat_byte(0x33)
    }),
    ("nonzero_nonce", |p| {
        p.header.nonce = [0, 0, 0, 0, 0, 0, 0, 1]
    }),
    // ── verifyHeaderGasFields legs (consensus/dummy/consensus.go:125-176):
    // each corrupts exactly ONE fee/gas equality check, leaving every
    // earlier-checked field untouched so the first rejection is the intended
    // one (order: GasLimit → ExtraPrefix → BaseFee → BlockGasCost → ExtData).
    ("wrong_gas_limit", |p| {
        p.header.gas_limit = p.header.gas_limit.saturating_add(1)
    }),
    ("tampered_fee_state_prefix", |p| {
        let mut extra = p.header.extra.to_vec();
        extra[9] ^= 0x01; // flip a bit inside the ACP-176 `excess` field
        p.header.extra = extra.into();
    }),
    ("wrong_base_fee", |p| {
        p.header.base_fee = p.header.base_fee.map(|bf| bf + U256::from(1))
    }),
    ("wrong_block_gas_cost", |p| {
        p.header.block_gas_cost = Some(U256::from(123u64))
    }),
    ("oversized_ext_data_gas_used", |p| {
        p.header.ext_data_gas_used = Some(U256::from(u64::MAX) + U256::from(1))
    }),
    // ── semanticVerify family (wrapped_block.go:335-391), Task 8. All four are
    // restamp-free (see the MUTATIONS doc above): the genesis fee state is at
    // its steady-state attractor, so shifting time fields leaves every fee/gas
    // recompute unchanged and isolates the one intended check.
    //
    // `missing_time_milliseconds`: forcing `None` while `min_delay_excess`
    // (t8) is present cannot omit the t7 slot — the encoder writes the nil
    // scalar `0x80`, which decodes back as `Some(0)` on BOTH sides (Go's
    // `rlp:"optional"` `*uint64` decoder does the same). The header then reads
    // as timestamped at the Unix epoch, which is EARLIER than the parent
    // genesis timestamp, so `verify_header_gas_fields`'s `feeStateBeforeBlock`
    // monotonic-time guard rejects it ("invalid fee state") one stage before
    // `VerifyTime`'s own `ErrTimeMillisecondsRequired` could fire — the same
    // reachability result Task 4 recorded (`ErrTimeMillisecondsRequired` is
    // only reachable via a direct `verify_time` call, covered by the feerules
    // unit test). Recorded honestly rather than forced to the brief's table.
    ("missing_time_milliseconds", |p| {
        p.header.time_milliseconds = None
    }),
    // `wrong_min_delay_excess`: an unreachable claim (min_delay_excess.go:73-79).
    // A bare header-tail field, not a fee-prefix input, so no other expectation
    // shifts — VerifyMinDelayExcess is the first rejection.
    ("wrong_min_delay_excess", |p| {
        p.header.min_delay_excess = Some(u64::MAX)
    }),
];

/// name -> (Go rejection substring, Rust rejection substring). Most pairs are
/// textually identical — Rust's `error.rs` sentinel messages are written to
/// mirror coreth's verbatim (`wrapped_block.go:398-527`) — but they are
/// checked independently against each side's own error text, so a future
/// divergence in either message would be caught rather than silently matching
/// a shared constant.
///
/// `missing_cancun_tail` is the one asymmetric pair: omitting
/// `parentBeaconRoot`/blob fields changes the header's RLP shape, so Go's
/// decoder rejects it structurally (`rlp: input string too short for
/// common.Hash, decoding into ...ParentBeaconRoot`) before ever reaching
/// `wrapped_block.go`'s semantic "missing parentBeaconRoot" check, whereas
/// Rust's decoder tolerates the shorter shape and rejects it later, at the
/// semantic check, with that exact sentinel. Both are correct rejections of
/// the same malformed candidate at different (equally valid) layers — the
/// same asymmetry Task 3 documented for non-empty uncle lists.
///
/// Task 6 adds five `verifyHeaderGasFields` pairs. One of them,
/// `oversized_ext_data_gas_used`, is a SECOND asymmetric pair, and the
/// recorded Go class differs from BOTH the plan's table prediction AND the
/// task brief's own predicted correction (neither survived contact with the
/// live oracle — see the Task 6 report for the full derivation):
///
/// * The plan's table expected coreth's `errExtDataGasUsedTooLarge`
///   ("extDataGasUsed is not uint64") — the LAST arm of
///   `verifyHeaderGasFields` itself (`consensus/dummy/consensus.go:29,160-175`).
/// * A later hypothesis expected `VerifyExtraPrefix`'s own fee-state
///   recompute (`feeStateAfterBlock` -> `acp176.State.ConsumeGas`) to reject
///   first with `gas.ErrInsufficientCapacity` ("insufficient capacity").
/// * What the LIVE judge actually recorded: coreth rejects earlier still, in
///   `wrappedBlock.verify` -> `semanticVerify` -> `verifyIntrinsicGas` ->
///   `customheader.VerifyGasUsed` (`customheader/gas_limit.go:60-90`) — a
///   check that runs BEFORE the consensus engine's `verifyHeaderGasFields` is
///   ever reached (that runs later still, inside `blockChain.InsertBlockManual`).
///   `VerifyGasUsed` holds the header's **unsaturated** `*big.Int`
///   `ExtDataGasUsed` and fails its own `!extDataGasUsed.IsUint64()` guard
///   with `errInvalidExtraDataGasUsed` ("invalid extra data gas used"),
///   wrapped as `errInvalidGasUsedRelativeToCapacity` ("invalid gas used
///   relative to capacity") by `verifyIntrinsicGas`, then "failed to verify
///   intrinsic gas" / "failed to verify block" by the callers above it.
///
/// Rust has never ported `VerifyGasUsed`/`verifyIntrinsicGas` at all (a
/// documented, intentional gap — see `feerules::verify_header_gas_fields`'s
/// own doc comment: "Go's `VerifyGasUsed` is NOT called here ... gas-used
/// correctness is checked by execution"), so Rust's rejection still comes
/// from `verify_extra_prefix`: `opt_u256_to_u64` *saturates* the oversized
/// value to `u64::MAX` (no "not uint64" guard is mirrored), and
/// `Acp176State::consume_gas` then fails the ordinary capacity check against
/// that saturated value, surfacing as [`Error::FeeOverflow`] ("fee
/// overflow"). Different message, different mechanism, different pipeline
/// stage on each side — but both are still gas-capacity/overflow rejections
/// of the SAME malformed field, at the earliest check each implementation
/// happens to run it through — not a struct-equality mismatch like
/// `tampered_fee_state_prefix`'s `IncorrectFeeState` / "incorrect fee state".
/// Task 8 appends the six `semanticVerify`-family classes. Five reject at the
/// full `parse_block → verify` entry like the rest; `understated_gas_used` needs
/// the verifying node BOOTSTRAPPED on both sides (Go's `verifyIntrinsicGas` is
/// `bootstrapped`-gated — `wrapped_block.go:375` — and the Go judge boots via
/// `vmtest.SetupTestVM`, which sets `snow.NormalOp`; Rust mirrors it by flipping
/// the VM to `NormalOp` before verifying), and `trailing_sae_tail_field` rejects
/// one stage earlier on the Rust side, at PARSE (`decode_rlp`'s trailing-bytes
/// fail-close) rather than at verify. Which Rust entry each candidate is driven
/// through is selected by [`rust_stage_for`]; the Go substrings below are the
/// classes the LIVE judge actually recorded (`verdicts.json`).
///
/// Two Task-8 Go classes were recorded honestly rather than forced to the
/// brief's prediction table (both anticipated by the task context):
///
/// * `missing_time_milliseconds` — the brief's table predicted
///   `TimeMilliseconds is required` on both sides, but through the wire that
///   mutation is indistinguishable from `Some(0)` (the t7 slot is forced by
///   the present t8 `min_delay_excess`, and `None` encodes as the same `0x80`
///   nil scalar both sides decode as `Some(0)`). The block then reads as
///   epoch-timestamped — earlier than its parent — and is rejected by the
///   fee-state monotonic-time guard BEFORE `VerifyTime`'s required-field arm is
///   reached, on both sides. Matched-but-earlier class, the same honest
///   asymmetry `oversized_ext_data_gas_used` documents above.
/// * `trailing_sae_tail_field` — Go decodes the spliced ninth header scalar as
///   the SAE `TargetExponent` and rejects it at `semanticVerify`'s
///   `VerifyTargetExponent` ("remote target exponent should be nil"); Rust's
///   `AvaHeader::decode_rlp` fail-closes on the trailing bytes at PARSE, one
///   stage earlier. Same verdict (the block is invalid), different stage — the
///   same parse-vs-semantic asymmetry `block.rs::trailing_sae_tail_field_fails_decode`
///   documents for the whole SAE tail-field family.
const REJECTION_CLASSES: [(&str, &str, &str); 16] = [
    (
        "zero_difficulty",
        "invalid difficulty",
        "invalid difficulty",
    ),
    (
        "missing_cancun_tail",
        "ParentBeaconRoot",
        "missing parentBeaconRoot",
    ),
    ("wrong_tx_root", "invalid txs hash", "invalid txs hash"),
    ("bad_coinbase", "invalid coinbase", "invalid coinbase"),
    ("nonzero_nonce", "invalid nonce", "invalid nonce"),
    ("wrong_gas_limit", "invalid gas limit", "invalid gas limit"),
    (
        "tampered_fee_state_prefix",
        "incorrect fee state",
        "incorrect fee state",
    ),
    ("wrong_base_fee", "expected base fee", "expected base fee"),
    (
        "wrong_block_gas_cost",
        "invalid block gas cost",
        "invalid block gas cost",
    ),
    (
        // Deviates from the plan's table (see the doc comment above): the
        // LIVE Go judge rejects inside `VerifyGasUsed`/`verifyIntrinsicGas`
        // (`errInvalidExtraDataGasUsed`, "invalid extra data gas used"), a
        // check Rust has never ported; Rust's own rejection comes from
        // `verify_extra_prefix`'s fee-state consumption overflowing
        // (`Error::FeeOverflow`, "fee overflow") instead.
        "oversized_ext_data_gas_used",
        "invalid extra data gas used",
        "fee overflow",
    ),
    // ── Task 8 semanticVerify family. The Go and Rust CHECK ORDERS differ:
    // Go runs `VerifyMinDelayExcess`/`VerifyTargetExponent`/`VerifyTime` inside
    // `semanticVerify` (`wrapped_block.go:345-366`) BEFORE the consensus
    // engine's `verifyHeaderGasFields` (which runs later, in the chain insert),
    // whereas Rust runs `verify_header_gas_fields` BEFORE `verify_time`
    // (`block.rs:988,1002`). For every mutant below whose broken field is the
    // one and only fault, both reach the same intended check and the classes
    // match verbatim. `missing_time_milliseconds` is the exception the order
    // difference exposes, recorded honestly (see the REJECTION_CLASSES doc).
    (
        // ASYMMETRIC (matched-but-earlier): the forced `None` round-trips as
        // `Some(0)` on both sides (t7 slot forced by the present t8), so the
        // header reads as epoch-timestamped — earlier than its parent. Go's
        // `VerifyTime` (semanticVerify, runs first) catches it as `errBlockTooOld`;
        // Rust's `verify_header_gas_fields` fee-state monotonic guard (runs
        // first on the Rust side) catches it as `InvalidFeeState`. Both reject
        // the same epoch-timestamp block at each side's earliest check.
        "missing_time_milliseconds",
        "block timestamp is too old",
        "invalid fee state",
    ),
    (
        "mismatched_time_milliseconds",
        "TimeMilliseconds does not match",
        "TimeMilliseconds does not match",
    ),
    (
        "far_future_time",
        "too far in the future",
        "too far in the future",
    ),
    (
        "wrong_min_delay_excess",
        "incorrect min delay excess",
        "incorrect min delay excess",
    ),
    (
        // Bootstrapped-gated on BOTH sides (see [`rust_stage_for`] +
        // `REJECTION_CLASSES` doc). Restamped so the extra prefix stays
        // consistent with `gas_used == 0`, isolating `verifyIntrinsicGas`.
        "understated_gas_used",
        "intrinsic gas",
        "intrinsic gas",
    ),
    (
        // ASYMMETRIC (matched-but-earlier stage): Go decodes the spliced ninth
        // header scalar as the SAE `TargetExponent` and rejects it at
        // `semanticVerify`'s `VerifyTargetExponent`; Rust's `decode_rlp`
        // fail-closes on the trailing bytes at PARSE. The Rust substring is
        // empty — for a parse-stage candidate the checker only requires that
        // `parse_block` itself errors (verify is never reached); the incidental
        // Rust message is the generic `rlp_err` mapping (`block.rs:1607`).
        "trailing_sae_tail_field",
        "remote target exponent",
        "",
    ),
];

/// Which Rust entry point each candidate is driven through in
/// [`proposer_verdicts_hold`]. Everything defaults to the full non-bootstrapped
/// `parse_block → verify` path; the two Task-8 exceptions are called out
/// explicitly (mirroring the Go judge, which boots bootstrapped via
/// `vmtest.SetupTestVM` and decodes the SAE tail field before its own semantic
/// checks).
#[derive(Clone, Copy, PartialEq, Eq)]
enum RustStage {
    /// `parse_block → verify`, node NOT bootstrapped (the common case).
    Verify,
    /// `parse_block → verify` with the VM flipped to `NormalOp` first — Go's
    /// `verifyIntrinsicGas` is `bootstrapped`-gated (`wrapped_block.go:375`).
    VerifyBootstrapped,
    /// `parse_block` alone must reject; `verify` is never reached (Rust
    /// fail-closes the SAE tail field one stage earlier than Go).
    Parse,
}

fn rust_stage_for(name: &str) -> RustStage {
    match name {
        "understated_gas_used" => RustStage::VerifyBootstrapped,
        "trailing_sae_tail_field" => RustStage::Parse,
        _ => RustStage::Verify,
    }
}

/// Decodes `base`, applies `mutate` to its parts, and re-assembles fresh
/// self-consistent wire bytes (the block hash is recomputed over the mutated
/// header) — the `cancun_clamp.rs:57-96` mutate+re-encode pattern, without the
/// proposervm-container unwrap step (our candidates are raw coreth block
/// bytes, not proposervm-wrapped).
fn mutate_candidate(spec: &AvaChainSpec, base: &[u8], mutate: fn(&mut AvaBlockParts)) -> Vec<u8> {
    let block = decode_ava_evm_block(base, spec).expect("decode honest candidate for mutation");
    let mut parts = block.into_parts();
    mutate(&mut parts);
    assemble_ava_block(parts, spec)
        .expect("assemble mutated candidate")
        .encoded_bytes()
        .to_vec()
}

/// Splices one extra RLP `u64` scalar (a would-be SAE `TargetExponent`, t9)
/// onto the END of the header list payload of an encoded coreth block, fixing
/// up both the header list-header and the outer block list-header. This is the
/// Task 7 technique (`block.rs::trailing_sae_tail_field_fails_decode`) lifted
/// to the whole-block level: `AvaBlockParts`/`assemble_ava_block` model no SAE
/// tail field, so the `trailing_sae_tail_field` mutant is built by raw byte
/// surgery on the honest bytes instead. Everything after the header (txs /
/// uncles / ext_data) is copied verbatim, so the mutant differs from the honest
/// block by exactly the one spliced field.
fn splice_trailing_header_field(block_bytes: &[u8]) -> Vec<u8> {
    // Outer block RLP list: [header, txs, uncles, ext_data?].
    let mut outer_cursor = block_bytes;
    let outer = RlpListHeader::decode(&mut outer_cursor).expect("decode outer block list");
    assert!(outer.list, "block must be an RLP list");
    // `outer_cursor` now points at the outer payload start (exactly
    // `payload_length` bytes, since nothing trails a whole block).
    let outer_payload = &outer_cursor[..outer.payload_length];

    // First outer element = the header list.
    let mut hdr_cursor = outer_payload;
    let hdr = RlpListHeader::decode(&mut hdr_cursor).expect("decode header list");
    assert!(hdr.list, "header must be an RLP list");
    let hdr_prefix_len = outer_payload.len() - hdr_cursor.len(); // header list-header bytes
    let hdr_total_len = hdr_prefix_len + hdr.payload_length;
    let hdr_payload = &outer_payload[hdr_prefix_len..hdr_total_len];
    let rest_of_outer = &outer_payload[hdr_total_len..]; // txs + uncles + ext_data, verbatim

    // Extended header payload = original header fields + one trailing u64.
    let mut new_hdr_payload = hdr_payload.to_vec();
    1u64.encode(&mut new_hdr_payload); // the trailing SAE field (t9)
    let mut new_hdr = Vec::new();
    RlpListHeader {
        list: true,
        payload_length: new_hdr_payload.len(),
    }
    .encode(&mut new_hdr);
    new_hdr.extend_from_slice(&new_hdr_payload);

    // Reassemble the outer block list around the extended header.
    let mut new_outer_payload = new_hdr;
    new_outer_payload.extend_from_slice(rest_of_outer);
    let mut out = Vec::new();
    RlpListHeader {
        list: true,
        payload_length: new_outer_payload.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&new_outer_payload);
    out
}

/// Decodes `honest_bytes`, applies `mutate`, RESTAMPS the ACP-176 `extra`
/// prefix so it stays consistent with the mutated header (recomputing
/// `feeStateAfterBlock` at the header's own `time`/`time_milliseconds`/`gas_used`
/// and splicing it over the first `STATE_SIZE` bytes, preserving the Durango
/// predicate-results suffix), re-assembles, and writes `<name>.rlp.hex`.
///
/// Used for the three restamp mutants (`understated_gas_used`,
/// `mismatched_time_milliseconds`, `far_future_time`): each corrupts one field
/// that ALSO feeds the fee-state recompute (`gas_used`, or the timestamp that
/// drives capacity growth), so without restamping Rust's earlier-running
/// `verify_header_gas_fields` would reject at `IncorrectFeeState` before the
/// intended `verifyIntrinsicGas`/`VerifyTime` check. Restamping keeps every
/// OTHER check satisfied (the Byzantine-proposer shape), isolating the one
/// intended rejection on BOTH sides. Mirrors
/// `semantic_verify.rs::understated_gas_used_block`.
fn emit_restamped_candidate(
    out_dir: &std::path::Path,
    name: &str,
    honest_bytes: &[u8],
    chain_spec: &AvaChainSpec,
    genesis_header: &AvaHeader,
    mutate: fn(&mut AvaBlockParts),
) {
    let block = decode_ava_evm_block(honest_bytes, chain_spec)
        .unwrap_or_else(|_| panic!("decode honest for {name}"));
    let mut parts = block.into_parts();
    mutate(&mut parts);
    let after = ava_evm::feerules::fee_state_after_block(
        chain_spec,
        genesis_header,
        parts.header.time,
        parts.header.time_milliseconds,
        parts.header.gas_used,
        0, // no atomic gas in these EVM-only candidates
        None,
    )
    .unwrap_or_else(|_| panic!("restamp fee state for {name}"));
    let mut extra = after.to_bytes().to_vec();
    extra.extend_from_slice(&parts.header.extra[ava_evm::feerules::acp176::STATE_SIZE..]);
    parts.header.extra = extra.into();
    let bytes = assemble_ava_block(parts, chain_spec)
        .unwrap_or_else(|_| panic!("assemble {name}"))
        .encoded_bytes()
        .to_vec();
    std::fs::write(out_dir.join(format!("{name}.rlp.hex")), hex::encode(&bytes))
        .unwrap_or_else(|e| panic!("write {name}.rlp.hex: {e}"));
}

/// Env-gated candidate writer (operator step). Builds the honest candidate —
/// a real block the Task 2-5 [`BlockBuilderDriver`] produces on the committed
/// local genesis, carrying one signed EVM tx from the pre-funded "ewoq"
/// key — asserts it passes Rust's own full `syntactic_verify` (never freeze a
/// corpus whose honest candidate Rust itself would reject), then emits it plus
/// the [`MUTATIONS`] and the four special-cased Task-8 mutants (three restamps
/// via [`emit_restamped_candidate`] + the raw-byte [`splice_trailing_header_field`])
/// as `<name>.rlp.hex` alongside a copy of the genesis JSON, into
/// `$EMIT_PROPOSER_CANDIDATES`.
#[test]
fn emit_proposer_candidates() {
    let Ok(out_dir) = std::env::var("EMIT_PROPOSER_CANDIDATES") else {
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| panic!("create {}: {e}", out_dir.display()));

    let genesis_json = local_genesis_json();
    let genesis = CChainGenesis::parse(genesis_json).expect("parse local genesis");
    let chain_spec = AvaChainSpec::c_chain(LOCAL_ID, Chain::from_id(genesis.chain_id()));
    let (genesis_bundle, genesis_bytecode) = genesis.genesis_alloc(chain_spec.network_upgrades());

    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open firewood");
    for (code_hash, code) in &genesis_bytecode {
        provider
            .bytecode_store()
            .put(code_hash.as_slice(), code)
            .expect("seed genesis bytecode");
    }
    let genesis_root = provider
        .propose_from_bundle(&genesis_bundle)
        .expect("propose genesis alloc");
    provider.commit(genesis_root).expect("commit genesis alloc");

    let genesis_header = genesis.genesis_header(genesis_root, chain_spec.network_upgrades());

    let config = AvaEvmConfig::new(chain_spec.clone());
    let canonical = Arc::new(CanonicalStore::new(Arc::new(MemDb::new())));
    let txpool = Arc::new(Mutex::new(AtomicMempool::new(64, Id::EMPTY)));
    let driver = BlockBuilderDriver::new(config.clone(), Arc::clone(&provider), txpool);

    // One EVM tx: a self-transfer from the pre-funded "ewoq" key, signed
    // EIP-155 over the genesis chain id (`sign_legacy` pattern, `evm_factory.rs`).
    let ewoq_key = PrivateKey::from_bytes(&hex::decode(EWOQ_KEY_HEX).expect("ewoq key hex"))
        .expect("ewoq key");
    let ewoq_addr = Address::from(ewoq_key.public_key().eth_address());
    let tx = TxLegacy {
        chain_id: Some(genesis.chain_id()),
        nonce: 0,
        gas_price: HONEST_TX_GAS_PRICE,
        gas_limit: 21_000,
        to: TxKind::Call(ewoq_addr),
        value: U256::from(1u64),
        input: Default::default(),
    };
    let sig_hash = tx.signature_hash();
    let rsv = ewoq_key.sign_hash(&sig_hash.0).expect("sign honest tx");
    let r = U256::from_be_slice(&rsv[..32]);
    let s = U256::from_be_slice(&rsv[32..64]);
    let sig = EvmSignature::new(r, s, rsv[64] == 1);
    let signed = TransactionSigned::Legacy(tx.into_signed(sig));
    let evm_tx = signed
        .try_into_recovered()
        .expect("recover honest tx sender");

    // Granite ACP-226 enforces a minimum inter-block delay derived from the
    // parent's `MinDelayExcess` (coreth `customheader/time.go:103-115`
    // `VerifyTime`); the local genesis carries `INITIAL_DELAY_EXCESS`, whose
    // `.delay()` is exactly 2000ms (`acp226.rs` golden table), so the child
    // must land >= 2 seconds after the genesis timestamp or Go's `Verify`
    // rejects it with "minimum block delay not met" — a real ACP-226 floor,
    // not a builder defect (Rust does not yet port this check itself).
    const MIN_BLOCK_DELAY_SECS: u64 = 2;
    let parent_time = genesis_header.time;
    let next_time = parent_time.saturating_add(MIN_BLOCK_DELAY_SECS);
    let ctx = AvaNextBlockCtx {
        timestamp: next_time,
        timestamp_ms: next_time.saturating_mul(1000),
        suggested_fee_recipient: Address::ZERO,
        parent_fee_state: parent_fee_state_of(config.chain_spec(), &genesis_header)
            .expect("parent fee state"),
        ..AvaNextBlockCtx::with_atomic_gas_limit(100_000)
    };

    let built = driver
        .build_on(&genesis_header, genesis_root, &ctx, vec![evm_tx])
        .expect("build honest candidate");
    assert_eq!(
        built.transactions().len(),
        1,
        "honest candidate must carry the one EVM tx"
    );

    // Self-check (never commit a corpus whose honest verdict is a rejection):
    // the honest candidate must pass Rust's own full `syntactic_verify` +
    // execution through the SAME `EvmBlock::verify` entry the `ChainVm`
    // adapter drives.
    let built_root = *built.header_state_root();
    provider.discard(built_root);
    let block_ctx = EvmBlockContext::new(Arc::clone(&provider), config, canonical);
    built
        .verify(&block_ctx, genesis_root, &genesis_header)
        .expect("honest candidate must pass Rust's own full syntactic_verify");

    let honest_bytes = built.encoded_bytes().to_vec();
    std::fs::write(out_dir.join("honest.rlp.hex"), hex::encode(&honest_bytes))
        .expect("write honest.rlp.hex");

    for (name, mutate) in MUTATIONS {
        let bytes = mutate_candidate(&chain_spec, &honest_bytes, mutate);
        std::fs::write(out_dir.join(format!("{name}.rlp.hex")), hex::encode(&bytes))
            .unwrap_or_else(|e| panic!("write {name}.rlp.hex: {e}"));
    }

    // ── Task 8 restamp mutants (see `emit_restamped_candidate`). Each corrupts
    // one field that also feeds the fee-state recompute, so the extra prefix is
    // restamped to keep every other check satisfied and isolate the intended
    // rejection — the Byzantine-proposer shape.
    //
    // `understated_gas_used`: gas_used below the single transfer's 21_000
    // intrinsic floor → isolates `verifyIntrinsicGas` (bootstrapped-gated).
    emit_restamped_candidate(
        &out_dir,
        "understated_gas_used",
        &honest_bytes,
        &chain_spec,
        &genesis_header,
        |p| p.header.gas_used = 0,
    );
    // `mismatched_time_milliseconds`: +5s in ms only → `Time != TimeMilliseconds/1000`
    // (time.go:94-101) → isolates `VerifyTime`'s mismatch arm.
    emit_restamped_candidate(
        &out_dir,
        "mismatched_time_milliseconds",
        &honest_bytes,
        &chain_spec,
        &genesis_header,
        |p| {
            let ms = p
                .header
                .time_milliseconds
                .expect("honest Granite header carries TimeMilliseconds");
            p.header.time_milliseconds = Some(ms.saturating_add(5_000));
        },
    );
    // `far_future_time`: year-4000 timestamp, deterministically beyond now+10s
    // for any real-clock run (time.go:72-79) → isolates `VerifyTime`'s
    // future-bound arm. Both `time` and `time_milliseconds` move together so
    // the Time==ms/1000 arm still holds and the future-bound arm fires first.
    emit_restamped_candidate(
        &out_dir,
        "far_future_time",
        &honest_bytes,
        &chain_spec,
        &genesis_header,
        |p| {
            const YEAR_4000: u64 = 64_060_588_800; // 4000-01-01 UTC, seconds.
            p.header.time = YEAR_4000;
            p.header.time_milliseconds = Some(YEAR_4000.saturating_mul(1000));
        },
    );

    // ── Task 8 raw-bytes mutant (`trailing_sae_tail_field`): splice one extra
    // RLP u64 (a would-be SAE `TargetExponent`) onto the header list — a shape
    // `AvaBlockParts`/`assemble_ava_block` cannot express. Rust rejects it at
    // PARSE (`decode_rlp` trailing-bytes fail-close); Go decodes it as
    // `TargetExponent` and rejects at `semanticVerify`'s `VerifyTargetExponent`.
    std::fs::write(
        out_dir.join("trailing_sae_tail_field.rlp.hex"),
        hex::encode(splice_trailing_header_field(&honest_bytes)),
    )
    .expect("write trailing_sae_tail_field.rlp.hex");

    // DEFERRED (Task 8 Step 6 — export-tx `inflated_ext_data_gas_used` leg):
    // an atomic-export-bearing candidate would give the ExtDataGasUsed
    // value-equality (`atomic/vm/block_extension.go:142` → `Tx.GasUsed`) a
    // cross-binary check. It cannot pass the Go judge OFFLINE: the Go judge
    // boots via `vmtest.SetupTestVM`, whose snow context takes `CChainID`,
    // `XChainID`, and `AVAXAssetID` from `ids.GenerateTestID()`
    // (`snow/snowtest/context.go:30-32` → `ids/test_generator.go:11`,
    // `Empty.Prefix(offset++)`) — process-counter-derived test IDs, NOT stable
    // well-known constants an offline Rust emitter can reproduce. An export
    // tx's `blockchain_id` must equal the VM's `CChainID`, its
    // `destination_chain` the `XChainID`, and its `asset_id` the `AVAXAssetID`;
    // built with any fixed IDs, coreth's atomic extension `SemanticVerify`
    // (`ExportTx.SemanticVerify` → `verifySpend`/`GetVerifiedAtomicUTXOs`)
    // rejects the block on a chain/asset mismatch (e.g. `errWrongChainID`)
    // BEFORE the `ExtDataGasUsed` equality is reached — a fixture reason
    // unrelated to this branch — so the "honest export candidate is accepted"
    // invariant cannot hold. The ExtDataGasUsed surface still has: (1) a
    // header-level cross-binary oracle leg via `oversized_ext_data_gas_used`
    // above (Go `invalid extra data gas used` / Rust `fee overflow`); and
    // (2) unit + golden-constant coverage (`atomic::verify::verify_ext_data_gas_used_arms`,
    // `cchain_atomic_tx::constants_match_go_vectors`). See PORTING.md's
    // `verify_ext_data_gas_used` row and the Task 8 report AS-BUILT note. To
    // lift this, the Go judge would need to inject FIXED chain/asset IDs into
    // its snow context (a judge-file change), tracked as a follow-up.

    std::fs::write(out_dir.join("genesis.json"), genesis_json).expect("write genesis.json");

    // 1 honest + the `fn`-based MUTATIONS + the four special-cased Task-8
    // mutants (understated_gas_used, mismatched_time_milliseconds,
    // far_future_time, trailing_sae_tail_field).
    let extra_task8 = 4;
    eprintln!(
        "wrote {} proposer-verdict candidates to {}",
        1 + MUTATIONS.len() + extra_task8,
        out_dir.display()
    );
}

#[derive(serde::Deserialize)]
struct VerdictEntry {
    name: String,
    accepted: bool,
    #[serde(default)]
    error: String,
}

#[derive(serde::Deserialize)]
struct VerdictsFile {
    verdicts: Vec<VerdictEntry>,
}

/// Drives Rust's own `EvmVm::parse_block` -> `Block::verify` entry (the same
/// one the `ChainVm` adapter drives) over `bytes`, booted on `genesis_json`.
/// Mirrors `cancun_clamp.rs`'s `parse_and_verify` helper.
async fn parse_and_verify(genesis_json: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the committed local genesis");
    let token = CancellationToken::new();
    let blk = vm
        .parse_block(&token, bytes)
        .await
        .map_err(|e| e.to_string())?;
    blk.verify(&token).await.map_err(|e| e.to_string())
}

/// As [`parse_and_verify`], but flips the VM to `NormalOp` (Go
/// `vm.bootstrapped.Set(true)`) before verifying, so `verify_intrinsic_gas`
/// (`wrapped_block.go:372-379`, bootstrapped-gated) actually runs — the Go judge
/// boots bootstrapped via `vmtest.SetupTestVM` (`SetState(snow.NormalOp)`), so
/// the Rust side must match. Mirrors `semantic_verify.rs::parse_and_verify_bootstrapped`.
async fn parse_and_verify_bootstrapped(genesis_json: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the committed local genesis");
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

/// Drives ONLY Rust's `EvmVm::parse_block` (never `verify`) over `bytes` —
/// used for the `trailing_sae_tail_field` candidate, which Rust fail-closes one
/// stage earlier than Go, at PARSE. Returns `Ok(())` if the bytes parse (which
/// makes the caller fail: a parse-stage candidate MUST be rejected at parse).
async fn parse_only(genesis_json: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let (vm, _genesis_id) = EvmVm::from_genesis(LOCAL_ID, dir.path(), genesis_json.as_bytes())
        .expect("EvmVm::from_genesis over the committed local genesis");
    let token = CancellationToken::new();
    vm.parse_block(&token, bytes)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Per-PR reader (TDD RED until the recording step writes `verdicts.json`):
/// loads the committed Go-oracle verdicts, asserts the honest candidate was
/// **accepted**, and for every adversarial candidate asserts BOTH the
/// recorded Go verdict is a rejection naming the expected sentinel AND that
/// Rust's own `parse_and_verify` rejects the identical bytes with the matching
/// sentinel — Go and Rust reject the SAME candidate for the SAME reason.
#[tokio::test]
async fn proposer_verdicts_hold() {
    let dir = corpus_dir();

    let verdicts_path = dir.join("verdicts.json");
    let raw = std::fs::read_to_string(&verdicts_path).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} (run the Task 6 recording step first: \
             EMIT_PROPOSER_CANDIDATES=... cargo test -p ava-evm --test proposer_candidates \
             -- --exact emit_proposer_candidates, then the Go judge)",
            verdicts_path.display()
        )
    });
    let file: VerdictsFile = serde_json::from_str(&raw).expect("parse verdicts.json");
    let by_name: BTreeMap<&str, &VerdictEntry> =
        file.verdicts.iter().map(|v| (v.name.as_str(), v)).collect();

    let genesis_json =
        std::fs::read_to_string(dir.join("genesis.json")).expect("read committed genesis.json");

    // The honest candidate: Go must ACCEPT it, and so must Rust's own entry.
    let honest = by_name
        .get("honest")
        .expect("verdicts.json carries a \"honest\" entry");
    assert!(
        honest.accepted,
        "coreth must ACCEPT the honest Rust-built block, got error: {:?}",
        honest.error
    );
    let honest_hex =
        std::fs::read_to_string(dir.join("honest.rlp.hex")).expect("read honest.rlp.hex");
    let honest_bytes = hex::decode(honest_hex.trim()).expect("decode honest.rlp.hex");
    parse_and_verify(&genesis_json, &honest_bytes)
        .await
        .expect("Rust must also accept the honest candidate via the ChainVm entry");

    // Every adversarial candidate: Go rejects with the expected sentinel, and
    // so does Rust, driven independently over the identical bytes.
    for (name, go_substr, rust_substr) in REJECTION_CLASSES {
        let go_verdict = by_name
            .get(name)
            .unwrap_or_else(|| panic!("verdicts.json is missing the {name:?} entry"));
        assert!(
            !go_verdict.accepted,
            "coreth must REJECT {name}, but it was accepted"
        );
        assert!(
            go_verdict.error.contains(go_substr),
            "{name}: Go error {:?} does not contain {go_substr:?}",
            go_verdict.error
        );

        let hex_str = std::fs::read_to_string(dir.join(format!("{name}.rlp.hex")))
            .unwrap_or_else(|e| panic!("read {name}.rlp.hex: {e}"));
        let bytes = hex::decode(hex_str.trim()).unwrap_or_else(|e| panic!("decode {name}: {e}"));

        // Drive Rust's own entry over the identical bytes, through whichever
        // stage this candidate is meant to be rejected at (see `rust_stage_for`).
        let rust_err = match rust_stage_for(name) {
            RustStage::Verify => parse_and_verify(&genesis_json, &bytes)
                .await
                .expect_err(&format!("Rust must also reject {name}")),
            RustStage::VerifyBootstrapped => parse_and_verify_bootstrapped(&genesis_json, &bytes)
                .await
                .expect_err(&format!("Rust (bootstrapped) must also reject {name}")),
            RustStage::Parse => parse_only(&genesis_json, &bytes).await.expect_err(&format!(
                "Rust must reject {name} at PARSE (verify is never reached)"
            )),
        };
        assert!(
            rust_err.contains(rust_substr),
            "{name}: Rust error {rust_err:?} does not contain {rust_substr:?}"
        );
    }
}
