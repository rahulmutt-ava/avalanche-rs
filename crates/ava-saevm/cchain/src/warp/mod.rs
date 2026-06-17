// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The SAE C-Chain Warp (ICM) message lifecycle (Go `vms/saevm/cchain/warp/`,
//! avalanchego `9b48abd852` / PR #5523 — spec 11 §8 upstream-delta).
//!
//! This is the asynchronous-C-Chain (ACP-194) mirror of the synchronous-C-Chain
//! warp machinery documented in spec 10 §6.5/§8. Four seams:
//!
//! - [`from_receipts`] — the **outbound** capture step: scan a block's receipts
//!   for `SendWarpMessage` logs at the warp precompile address and unpack each
//!   into an [`UnsignedMessage`]. Under SAE this runs *after* the block executes
//!   (execution follows acceptance). Reuses
//!   [`ava_evm::precompile::warp::unpack_send_warp_event_data_to_message`] — the
//!   event unpacking is NOT re-implemented here.
//! - [`Storage`] — the warp message store (persist/cache + off-chain overrides).
//! - [`Verifier`] — the ACP-118 sign-decision (the four refusal codes).
//! - [`verify_block`] — the **inbound** predicate pass: BLS-verify the warp
//!   predicates of every tx in a block, collecting per-precompile failures into
//!   [`BlockResults`]. Reuses the M6 synchronous predicate machinery
//!   ([`ava_evm::precompile::warp`]); fans the per-predicate BLS verify out via
//!   `rayon` (Go uses an `errgroup`).
//!
//! **Non-gating** (Helicon is unscheduled and SAE C-Chain warp interop is not yet
//! exercised) — correct-but-dormant parity with Go.

pub mod storage;
pub mod verifier;

use std::collections::BTreeMap;

use ava_evm::precompile::warp::{
    PredicateContext, WARP_PRECOMPILE_ADDRESS, unpack_send_warp_event_data_to_message,
    warp_predicates_from_tx,
};
use ava_evm_reth::{Address, B256, RecoveredTx};
use ava_types::constants::PRIMARY_NETWORK_ID;
use ava_types::id::Id;
use ava_utils::bits::Bits;
use ava_validators::state::{ValidatorState, WarpSet};
use ava_warp::payload::WarpPayload;
use ava_warp::verifier::{
    WARP_QUORUM_DENOMINATOR, WARP_QUORUM_NUMERATOR, verify_bit_set_signature,
};
use ava_warp::{Message, Signature, UnsignedMessage};
use rayon::prelude::*;

pub use storage::Storage;
pub use verifier::{AppError, AppErrorCode, Backend, Verifier};

/// The SAE C-Chain warp lifecycle errors (Go returns wrapped `error`s).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `no block context` — predicates are present but no proposervm
    /// [`block::Context`](BlockContext) was supplied (Go `errNoBlockContext`).
    #[error("no block context")]
    NoBlockContext,
    /// A receipt log failed to unpack into a warp message (Go's wrapped
    /// "parsing log data into warp message" error).
    #[error("parsing log data into warp message: {0}")]
    Warp(#[from] ava_warp::Error),
    /// A storage / DB operation failed.
    #[error("warp storage: {0}")]
    Db(#[from] ava_database::Error),
    /// A validator-state lookup failed during [`verify_block`].
    #[error("validator state: {0}")]
    ValidatorState(#[from] ava_validators::error::Error),
}

/// The proposervm `block.Context` the predicate pass requires (Go
/// `snow/engine/snowman/block.Context`). It pins the P-Chain height used to
/// select the warp validator set. Modeled as a present-or-absent value so the
/// nil-context gate ([`Error::NoBlockContext`]) is faithful to Go.
#[derive(Clone, Copy, Debug)]
pub struct BlockContext {
    /// `PChainHeight` — the P-Chain height the proposervm block was issued at.
    pub pchain_height: u64,
}

/// `predicate.PrecompileResults` — per-precompile-address failure bits for one
/// transaction (Go `map[common.Address]set.Bits`). A set bit at index `i` means
/// the `i`-th predicate addressed to that precompile FAILED verification.
pub type PrecompileResults = BTreeMap<Address, Bits>;

/// `predicate.BlockResults` — per-transaction predicate results for a block (Go
/// `map[common.Hash]predicate.PrecompileResults`), keyed by transaction hash.
pub type BlockResults = BTreeMap<B256, PrecompileResults>;

/// `FromReceipts(receipts)` — the outbound messages included in `receipts`
/// (Go `cchain/warp/warp.go`).
///
/// Scans every log in every receipt for the warp precompile
/// [`WARP_PRECOMPILE_ADDRESS`] and unpacks its `SendWarpMessage` `data` into an
/// [`UnsignedMessage`], preserving receipt-then-log order. Non-warp logs are
/// ignored. Each receipt is a slice of its logs (the SAE driver maps reth
/// receipt logs onto this view).
///
/// # Errors
/// Returns [`Error::Warp`] if a warp-addressed log's data does not unpack into a
/// valid unsigned warp message.
pub fn from_receipts(receipts: &[Vec<ReceiptLog>]) -> Result<Vec<UnsignedMessage>, Error> {
    let mut messages = Vec::new();
    for logs in receipts {
        for log in logs {
            if log.address != WARP_PRECOMPILE_ADDRESS {
                continue;
            }
            let m = unpack_send_warp_event_data_to_message(&log.data)?;
            messages.push(m);
        }
    }
    Ok(messages)
}

/// A single block-receipt log, reduced to the fields [`from_receipts`] reads
/// (the emitting address + the ABI-encoded `SendWarpMessage` data). The SAE
/// driver builds these from the executed block's reth receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptLog {
    /// The emitting contract address (compared against [`WARP_PRECOMPILE_ADDRESS`]).
    pub address: Address,
    /// The log `data` (`abi.encode(bytes message)` for a `SendWarpMessage` log).
    pub data: Vec<u8>,
}

/// `VerifyBlock(snowCtx, blockCtx, rules, txs)` — verify the warp predicates of
/// every transaction in a block (Go `cchain/warp/warp.go`).
///
/// For each tx, the warp predicates riding in its access list
/// ([`warp_predicates_from_tx`]) are BLS-aggregate-verified against the source
/// subnet's [`WarpSet`] at the proposervm-pinned P-Chain height. The result is a
/// [`BlockResults`] mapping each tx (with predicates) to the per-precompile bits
/// of FAILED predicate indices — a fully-valid tx maps to an empty [`Bits`].
///
/// `block_context` MUST be `Some` when any tx carries predicates; otherwise
/// [`Error::NoBlockContext`] is returned. This check sits inside the per-tx loop
/// so a block with no predicates does not require a block context (matching Go).
///
/// The per-predicate BLS verification is fanned out across `rayon` (Go uses an
/// `errgroup` capped at `GOMAXPROCS`); the validator-set lookups happen once up
/// front because this repo's [`ValidatorState`] is async.
///
/// # Errors
/// - [`Error::NoBlockContext`] if predicates are present but `block_context` is
///   `None`.
/// - [`Error::ValidatorState`] if a validator-set lookup fails (a node-level
///   error, not a per-predicate verification failure).
pub async fn verify_block<V: ValidatorState>(
    ctx: &PredicateContext,
    block_context: Option<BlockContext>,
    state: &V,
    txs: &[RecoveredTx],
) -> Result<BlockResults, Error> {
    // 1. Extract per-tx predicates, applying the nil-block-context gate only when
    //    a tx actually carries predicates (matching Go's in-loop check).
    let mut per_tx: Vec<(B256, Vec<Vec<u8>>)> = Vec::with_capacity(txs.len());
    for tx in txs {
        let predicates = warp_predicates_from_tx(tx);
        if predicates.is_empty() {
            continue;
        }
        if block_context.is_none() {
            return Err(Error::NoBlockContext);
        }
        per_tx.push((*tx.tx_hash(), predicates));
    }
    if per_tx.is_empty() {
        return Ok(BlockResults::new());
    }

    // 2. Resolve the validator-set data once (async), then verify each predicate
    //    in parallel (sync, rayon) — the source-subnet map for every distinct
    //    source chain + the warp validator sets at the pinned P-Chain height.
    let pchain_height = block_context.ok_or(Error::NoBlockContext)?.pchain_height;
    let resolved = resolve_validator_data(ctx, state, &per_tx, pchain_height).await?;

    // 3. Fan the per-predicate BLS verification out over rayon, collecting the
    //    failure bits per (tx, precompile-address).
    let results: Vec<(B256, Bits)> = per_tx
        .par_iter()
        .map(|(tx_hash, predicates)| {
            let mut failures = Bits::new();
            for (i, chunks) in predicates.iter().enumerate() {
                if !verify_one_sync(ctx, chunks, &resolved) {
                    failures.add(u64::try_from(i).unwrap_or(u64::MAX));
                }
            }
            (*tx_hash, failures)
        })
        .collect();

    let mut block_results = BlockResults::new();
    for (tx_hash, failures) in results {
        let mut precompile_results = PrecompileResults::new();
        precompile_results.insert(WARP_PRECOMPILE_ADDRESS, failures);
        block_results.insert(tx_hash, precompile_results);
    }
    Ok(block_results)
}

/// The validator-set data resolved up front (async) so the per-predicate BLS
/// verification can run synchronously under `rayon`.
struct ResolvedValidatorData {
    /// `source_chain_id → source_subnet_id` for every distinct source chain.
    subnet_ids: BTreeMap<Id, Id>,
    /// The warp validator sets at the pinned P-Chain height, keyed by subnet id.
    sets: BTreeMap<Id, WarpSet>,
}

/// Resolves the source-subnet of every distinct source chain referenced by the
/// block's predicates plus the warp validator sets at `pchain_height`.
async fn resolve_validator_data<V: ValidatorState>(
    ctx: &PredicateContext,
    state: &V,
    per_tx: &[(B256, Vec<Vec<u8>>)],
    pchain_height: u64,
) -> Result<ResolvedValidatorData, Error> {
    let mut subnet_ids: BTreeMap<Id, Id> = BTreeMap::new();
    for (_, predicates) in per_tx {
        for chunks in predicates {
            let Some(source_chain) = source_chain_of(chunks) else {
                continue;
            };
            if subnet_ids.contains_key(&source_chain) {
                continue;
            }
            let subnet = state.get_subnet_id(source_chain).await?;
            subnet_ids.insert(source_chain, subnet);
        }
    }
    let sets_map = state.get_warp_validator_sets(pchain_height).await?;
    let _ = ctx;
    let sets = sets_map.into_iter().collect();
    Ok(ResolvedValidatorData { subnet_ids, sets })
}

/// The source chain id of a predicate's warp message, or `None` if the predicate
/// is structurally invalid (it then reads as a failed predicate).
fn source_chain_of(chunks: &[u8]) -> Option<Id> {
    let raw = ava_evm::precompile::warp::predicate_from_chunks(chunks)?;
    let msg = Message::parse(&raw).ok()?;
    Some(msg.unsigned_message.source_chain_id)
}

/// `VerifyPredicate(pc, pred)` — verify one warp predicate against the
/// pre-resolved validator data (the synchronous, `rayon`-parallel core).
///
/// Returns `true` iff the predicate verified. A structurally-invalid predicate,
/// an unresolvable source subnet/validator set, or a failed quorum all read as
/// `false` (Go: a failed predicate is recorded as a set bit, not a block error).
fn verify_one_sync(
    ctx: &PredicateContext,
    chunks: &[u8],
    resolved: &ResolvedValidatorData,
) -> bool {
    // 1. Decode chunks → raw bytes → `Message`.
    let Some(raw) = ava_evm::precompile::warp::predicate_from_chunks(chunks) else {
        return false;
    };
    let Ok(msg) = Message::parse(&raw) else {
        return false;
    };
    // The payload must parse (coreth validates it before quorum).
    if WarpPayload::parse(&msg.unsigned_message.payload).is_err() {
        return false;
    }

    // 2. Resolve the source subnet (and the substitution branch, step 3).
    let source_chain_id = msg.unsigned_message.source_chain_id;
    let Some(&source_subnet) = resolved.subnet_ids.get(&source_chain_id) else {
        return false;
    };
    let source_subnet = if source_subnet == PRIMARY_NETWORK_ID
        && (!ctx.require_primary_network_signers || source_chain_id == Id::EMPTY)
    {
        ctx.local_subnet_id
    } else {
        source_subnet
    };

    // 3. Resolve the source subnet's `WarpSet`.
    let Some(warp_set) = resolved.sets.get(&source_subnet) else {
        return false;
    };

    // 4. BLS-aggregate verify against the set.
    let Signature::BitSet(sig) = &msg.signature;
    let quorum_numerator = if ctx.quorum_numerator == 0 {
        WARP_QUORUM_NUMERATOR
    } else {
        ctx.quorum_numerator
    };
    verify_bit_set_signature(
        sig,
        &msg.unsigned_message,
        ctx.network_id,
        warp_set,
        quorum_numerator,
        WARP_QUORUM_DENOMINATOR,
    )
    .is_ok()
}
