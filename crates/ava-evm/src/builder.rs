// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `BlockBuilderDriver`: on-demand block build with precomputed-root `finish`
//! (G5, spec 10 §4/§17.6).
//!
//! ## Why a custom driver (not reth's `PayloadBuilderService`)
//!
//! Vanilla reth builds payloads on the engine's timer via
//! `PayloadBuilderService`/`PayloadJob`. coreth builds **only when consensus
//! asks** (`BuildBlock`) and only when the mempool has work
//! (`block_builder.go`: `needToBuild`, `signalCanBuild`, a min-retry delay). We
//! reproduce coreth's model — `ChainVm::build_block` calls
//! [`BlockBuilderDriver::build_on`], which respects the
//! `minBlockBuildingRetryDelay` guard and pulls one atomic batch + the
//! highest-effective-tip EVM txs into the next block (spec 10 §4).
//!
//! ## Build-then-verify symmetry (the determinism contract, §17.6)
//!
//! `build_on` drives the **same** [`AvaEvmConfig`] executor over the **same**
//! parent Firewood view with the **same** [`AtomicStateHook`] pre-hook as
//! [`EvmBlock::verify`](crate::block::EvmBlock::verify) (§3.2). It then sets the
//! built header's `state_root` to the Firewood pre-commit root it computed, so a
//! self-built block re-verifies to the identical root.
//!
//! ## G5/G1 precomputed-root `finish`
//!
//! The "precomputed-root finish" the spec sketches as
//! `builder.finish(view_tip, Some((root, TrieUpdates::default())))` is realized
//! here through the [`FirewoodStateProvider`] proposal lifecycle, **not** reth's
//! `BlockBuilder::finish`: [`FirewoodStateProvider::propose_from_bundle`] already
//! computes the real Firewood root and **stashes** the deterministic proposal ops
//! keyed by that root (the empty-`TrieUpdates` half of the G1 trick lives in
//! `state.rs::state_root_with_updates`). reth never computes or persists a trie
//! root. The stashed proposal is committed verbatim on
//! [`accept`](crate::block::EvmBlock::accept). This mirrors the as-built §17.2.2
//! deviation (we reuse the `execute_batch` + `propose_from_bundle` path that
//! `verify` uses, guaranteeing symmetry by construction) rather than threading
//! reth's vN-sensitive `builder_for_next_block`/`finish` seam.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ava_evm_reth::{
    B256, Bloom, Bytes, ConsensusTx, EMPTY_OMMER_ROOT_HASH, EthReceipt, ExternalConsensusExecutor,
    Header, RecoveredTx, State, StateBuilder, StateProviderDatabase, TransactionSigned, TxReceipt,
    U256, calculate_transaction_root, keccak256,
};
use parking_lot::Mutex;

use crate::atomic::hook::AtomicStateHook;
use crate::atomic::mempool::AtomicMempool;
use crate::atomic::tx::{AtomicTx, CODEC_VERSION, Tx as SignedAtomicTx, codec as atomic_codec};
use crate::block::{AvaBlockParts, AvaHeader, EvmBlock, assemble_ava_block, empty_ext_data_hash};
use crate::chainspec::AvaPhase;
use crate::error::{Error, Result};
use crate::evmconfig::{AvaEvmConfig, AvaNextBlockCtx};
use crate::feerules::atomic_fee;
use crate::feerules::blockgas::{BLOCK_GAS_COST_STEP_AP4, BLOCK_GAS_COST_STEP_AP5, block_gas_cost};
use crate::precompile::rewardmanager::BLACKHOLE_ADDRESS;
use crate::state::FirewoodStateProvider;

/// coreth `block_builder.go::minBlockBuildingRetryDelay` — the minimum wall-clock
/// delay between two build attempts **on the same parent**. The driver returns
/// the "nothing to build" shape (mapped by the VM to the `ErrNoPendingBlock`
/// no-op) if asked to re-build the same parent sooner.
pub const MIN_BLOCK_BUILD_DELAY: Duration = Duration::from_millis(500);

/// The avalanchego-linearcodec encoding of an **empty** `predicate.BlockResults`
/// map (`vms/evm/predicate/results.go` `BlockResults.Bytes()`): a 2-byte codec
/// version (`0`) followed by a 4-byte big-endian element count (`0`) — 6 zero
/// bytes. Post-Durango, coreth's `core/evm.go:187` (`SetPredicateBytesInExtra`)
/// always appends this suffix to `header.Extra` (after the ACP-176/window
/// prefix), even when the block has no warp-predicated EVM txs at all — an
/// absent suffix is a syntactic-verification rejection
/// (`wrapped_block.go:556-560`, `errInvalidHeaderPredicateResults`), not merely
/// a missed optimization.
///
/// M9.15 Task 6's live differential (coreth judging a Rust-built block) caught
/// this: the driver did not append any suffix, so Go rejected an otherwise
/// honest block. Real per-tx warp-predicate verification
/// ([`crate::precompile::warp::build_block_predicates`]) needs a
/// [`ava_validators::state::ValidatorState`] handle this driver does not hold
/// (it is wired only into the verify path today, M6.31); until that pass is
/// threaded into `build_on` (deferred: M6.23 reth-txpool EVM-tx inclusion,
/// which is the only way an EVM tx reaches a built block today, carries no
/// warp-predicated access lists), every block this driver can build has zero
/// predicated results, so this constant is the byte-exact CORRECT value, not a
/// placeholder.
const EMPTY_BLOCK_PREDICATE_RESULTS: [u8; 6] = [0u8; 6];

/// The block-gas-cost regime the AP4+ surcharge runs under for the next block,
/// resolved from the active phase at the build timestamp.
struct BlockGasCostParams {
    /// Whether the AP4 block-gas-cost surcharge is active (AP4+).
    active: bool,
    /// The per-second step (AP4 vs AP5+).
    step: u64,
    /// Whether Granite has retired the mechanism (then the cost is forced to 0).
    granite: bool,
}

/// On-demand C-Chain block builder (coreth `block_builder.go` + `miner`).
///
/// Holds the same collaborators the verify path uses (state provider + EVM
/// config) plus the atomic mempool the batch is pulled from and the
/// `minBlockBuildingRetryDelay` guard. The driver is `Notify`-driven: the VM
/// parks on [`AtomicMempool::subscribe`] and calls [`Self::build_on`] when work
/// arrives (spec 10 §4).
pub struct BlockBuilderDriver {
    /// The EVM config (the bare reth `BlockExecutor` driver) — the **same**
    /// instance `verify` runs against, so build-then-verify is symmetric.
    evm_config: AvaEvmConfig,
    /// The Firewood state-of-record provider (parent view + propose/stash).
    state: Arc<FirewoodStateProvider>,
    /// The atomic X<->C mempool the per-block batch is drained from.
    txpool: Arc<Mutex<AtomicMempool>>,
    /// `(parent, last-build-instant)` — the min-retry-delay guard
    /// (coreth `lastBuildTime` keyed on the parent).
    last_build: Mutex<Option<(B256, Instant)>>,
}

impl BlockBuilderDriver {
    /// Builds a driver over its collaborators (the VM clones its own `Arc`s in).
    #[must_use]
    pub fn new(
        evm_config: AvaEvmConfig,
        state: Arc<FirewoodStateProvider>,
        txpool: Arc<Mutex<AtomicMempool>>,
    ) -> Self {
        Self {
            evm_config,
            state,
            txpool,
            last_build: Mutex::new(None),
        }
    }

    /// Whether a build on `parent` is allowed now (the
    /// `minBlockBuildingRetryDelay` guard). A first build on a parent, or one
    /// after the delay has elapsed, is allowed; a re-build of the same parent
    /// within the delay is not (coreth `block_builder.go`).
    #[must_use]
    pub fn can_build_on(&self, parent: B256, now: Instant) -> bool {
        match *self.last_build.lock() {
            Some((last_parent, last_at)) if last_parent == parent => {
                now.duration_since(last_at) >= MIN_BLOCK_BUILD_DELAY
            }
            _ => true,
        }
    }

    /// **On-demand build** (spec 10 §4/§17.6, the `ChainVm::build_block` seam).
    ///
    /// Drives the same executor/pre-hook/parent-view as `verify`, packs one
    /// atomic batch + the supplied EVM txs (already in descending effective-tip
    /// order) under the gas + `blockGasCost` budget, computes the Firewood
    /// pre-commit root (stashed by [`FirewoodStateProvider::propose_from_bundle`]
    /// for commit-on-accept), and assembles the byte-exact coreth block whose
    /// `header.state_root` is that root.
    ///
    /// `parent` is the parent header (the build target's `number - 1`);
    /// `parent_state_root` is the committed Firewood root the EVM executes
    /// against; `ctx` carries the next-block fee/timestamp/atomic-gas inputs
    /// ([`AvaNextBlockCtx`], §17.3); `evm_txs` are candidate EVM txs ordered by
    /// fee cap (the caller supplies them — `vm.rs::build_block` drains the
    /// purpose-built `crate::mempool::EvmMempool` via `best_txs()`; there is no
    /// reth `TransactionPool` in ava-evm).
    ///
    /// # Errors
    /// Returns [`Error::MissingProposal`] (the "nothing to build" no-op) when the
    /// min-retry-delay guard rejects the attempt or there is no work (no atomic
    /// batch and no includable EVM tx). Other [`Error`]s surface a genuine
    /// execution / fee-budget / Firewood failure.
    pub fn build_on(
        &self,
        parent: &AvaHeader,
        parent_state_root: B256,
        ctx: &AvaNextBlockCtx,
        evm_txs: Vec<RecoveredTx>,
    ) -> Result<EvmBlock> {
        let parent_hash = parent.hash();

        // 1. min-retry-delay guard (coreth `needToBuild`/`signalCanBuild`).
        if !self.can_build_on(parent_hash, Instant::now()) {
            return Err(Error::MissingProposal(parent_hash));
        }

        // 2. Pull one gas-limited atomic batch (coreth one-batch-per-block, §6.4).
        let atomic_batch = self.txpool.lock().next_batch(ctx);

        // 3. The base fee + gas limit for the next block (§7.2/§17.3). The
        //    per-fork override (`next_evm_env`) consumes a reth `Header`, so we
        //    project the coreth parent header onto the fee-bearing reth fields.
        let parent_eth = parent_eth_header(parent)?;
        let next_env = self.evm_config.next_evm_env(&parent_eth, ctx)?;
        let base_fee = next_env.evm_env.block_env.basefee;
        let gas_limit = next_env.evm_env.block_env.gas_limit;

        // 4. Reserve atomic gas against the block budget BEFORE packing EVM txs
        //    (spec 10 §7.3/§17.3, 21 §4b): the atomic batch always goes first.
        let hook = AtomicStateHook::new(unsigned_of(&atomic_batch));
        let atomic_tx_lens = atomic_tx_lens(&atomic_batch);
        let atomic_gas_used = hook.batch_gas(&atomic_tx_lens)?;

        // 5. The blockGasCost surcharge (AP4+, spec 21 §4b). We fold it into the
        //    gas budget so a faster-than-target block leaves proportionally less
        //    room for EVM txs (coreth `customheader.BlockGasCostWithStep`).
        let bgc = self.block_gas_cost_params(ctx);
        let block_gas_cost_value = if bgc.active {
            block_gas_cost(
                parent.block_gas_cost.map(u256_to_u64),
                bgc.step,
                ctx.timestamp.saturating_sub(parent.time),
                bgc.granite,
            )
        } else {
            0
        };

        // 6. Pack EVM txs by effective tip until the gas budget is hit, skipping
        //    txs that don't fit (coreth `miner` loop). The budget is the block gas
        //    limit less the reserved atomic gas and the blockGasCost reservation.
        let reserved = atomic_gas_used.saturating_add(block_gas_cost_value);
        let evm_budget = gas_limit.saturating_sub(reserved);
        let included = self.pack_evm_txs(parent, ctx, base_fee, evm_budget, evm_txs);

        // Nothing to issue (no atomic batch, no includable EVM tx): the engine
        // treats this as "the VM does not want to build" (ErrNoPendingBlock).
        if atomic_batch.is_empty() && included.txs.is_empty() {
            return Err(Error::MissingProposal(parent_hash));
        }

        // 7. Execute the packed batch with the atomic pre-hook over the parent
        //    Firewood view — the SAME path as verify (§3.2), so the resulting
        //    bundle (and therefore the root) is identical on re-verify.
        let view = self.state.history_by_state_root(parent_state_root)?;
        let mut overlay: State<StateProviderDatabase<_>> = StateBuilder::new()
            .with_database(StateProviderDatabase::new(view))
            .with_bundle_update()
            .build();
        let env = self.evm_config.evm_env_for_header(&included.env_header);
        let outcome = self
            .evm_config
            .execute_batch(env, &mut overlay, &hook, &included.txs)?;

        // 8. Firewood pre-commit root: propose the bundle (NOT committed) and
        //    stash the ops keyed by root (G5/G1 precomputed-root trick, §17.2.2).
        //    reth never computes or persists a trie root.
        let state_root = self.state.propose_from_bundle(&outcome.bundle)?;

        // 9. The atomic fee the batch must pay at the next-block base fee
        //    (ErrFeeOverflow guard, §17.3) — computed for parity even though the
        //    pre-hook already moved the balances.
        let _atomic_fee = atomic_fee(atomic_gas_used, U256::from(base_fee))?;

        // 9b. coreth `customtypes/block_ext.go:189` — `NewBlockWithExtData`
        //     derives TxHash/ReceiptHash/Bloom from the body, never sentinels.
        //     `transactions` is collected here (moving `included.txs`) so it can
        //     feed BOTH the tx-root calculation below and `AvaBlockParts` at
        //     assembly without a clone.
        let transactions: Vec<TransactionSigned> = included
            .txs
            .into_iter()
            .map(RecoveredTx::into_inner)
            .collect();
        let tx_root = calculate_transaction_root(&transactions);
        // Same bloom-recomputing ordered-trie helper `ava-saevm-exec::driver.rs:269`
        // uses for its `receipt_root` (mirrors it exactly — see the facade
        // re-export comment in `ava-evm-reth::lib.rs`).
        let receipt_root = EthReceipt::calculate_receipt_root_no_memo(&outcome.result.receipts);
        let bloom = outcome
            .result
            .receipts
            .iter()
            .fold(Bloom::ZERO, |acc, r| acc | TxReceipt::bloom(r));

        // 10. Assemble the byte-exact coreth block (§9.3) whose header.state_root
        //     is the Firewood root, so the self-built block re-verifies to it.
        let header = self.build_header(BuildHeaderArgs {
            parent,
            parent_hash,
            ctx,
            state_root,
            base_fee,
            gas_limit,
            gas_used: outcome.result.gas_used,
            block_gas_cost: block_gas_cost_value,
            ap4_active: bgc.active,
            atomic_gas_used,
            atomic_batch: &atomic_batch,
            tx_root,
            receipt_root,
            bloom,
        })?;
        let parts = AvaBlockParts {
            header,
            transactions,
            atomic_txs: atomic_batch.clone(),
            ext_data: ext_data_of(&atomic_batch)?,
            version: 0,
        };
        let block = assemble_ava_block(parts, self.evm_config.chain_spec())?;

        // 11. Record the build so the min-retry-delay guard rejects an immediate
        //     re-build on the same parent (coreth `lastBuildTime`).
        *self.last_build.lock() = Some((parent_hash, Instant::now()));
        Ok(block)
    }

    /// Packs EVM txs in the supplied (effective-tip-descending) order until the
    /// gas budget is hit, skipping txs that individually overflow the remaining
    /// budget. Returns the included txs and the env header they execute under.
    fn pack_evm_txs(
        &self,
        parent: &AvaHeader,
        ctx: &AvaNextBlockCtx,
        base_fee: u64,
        gas_budget: u64,
        candidates: Vec<RecoveredTx>,
    ) -> PackedEvm {
        let mut included: Vec<RecoveredTx> = Vec::new();
        let mut used: u64 = 0;
        for tx in candidates {
            // Skip txs that cannot pay the block base fee (no positive tip).
            if ConsensusTx::effective_tip_per_gas(tx.inner(), base_fee).is_none() {
                continue;
            }
            let tx_gas = ConsensusTx::gas_limit(tx.inner());
            let Some(next_used) = used.checked_add(tx_gas) else {
                continue;
            };
            if next_used > gas_budget {
                // Over budget with this tx; keep scanning for a smaller one.
                continue;
            }
            used = next_used;
            included.push(tx);
        }

        // The Cancun tail MUST be carried into the execution env header, not
        // just the assembled `AvaHeader`: alloy-evm's beacon-root system call
        // errors with `MissingParentBeaconBlockRoot` for a Cancun-active block
        // whose env header lacks the root (the same requirement the verify
        // path's `eth_env_header` documents — coreth activates Cancun with
        // Etna, so every such block carries `parentBeaconRoot = 0x0` and runs
        // `ProcessBeaconBlockRoot`, `core/state_processor.go`).
        let phase = self.evm_config.chain_spec().fork_at(ctx.timestamp);
        let (blob_gas_used, excess_blob_gas, parent_beacon_block_root) = cancun_tail(phase);

        let env_header = Header {
            parent_hash: parent.hash(),
            number: parent.number.saturating_add(1),
            timestamp: ctx.timestamp,
            // The gas limit the env executes under (the block ceiling). The
            // executor enforces per-tx gas; this is the block bound.
            gas_limit: parent
                .gas_limit
                .max(used)
                .max(gas_budget.saturating_add(used)),
            base_fee_per_gas: Some(base_fee),
            // coreth executes with the etherbase (= blackhole) as coinbase, so
            // priority fees accrue to the blackhole (`plugin/evm/vm.go:565`);
            // leaving `suggested_fee_recipient` here would diverge the state
            // root from what Go computes for the same block.
            beneficiary: BLACKHOLE_ADDRESS,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_block_root,
            ..Default::default()
        };
        PackedEvm {
            txs: included,
            env_header,
        }
    }

    /// Builds the coreth [`AvaHeader`] for the next block. The fork-gated AP3
    /// base-fee field and the AP4 `ext_data_gas_used`/`block_gas_cost` tail are
    /// populated from the resolved phase; `ext_data_hash` is the atomic-batch
    /// commitment (or the empty sentinel when there are no atomic txs).
    fn build_header(&self, args: BuildHeaderArgs<'_>) -> Result<AvaHeader> {
        let BuildHeaderArgs {
            parent,
            parent_hash,
            ctx,
            state_root,
            base_fee,
            gas_limit,
            gas_used,
            block_gas_cost: block_gas_cost_value,
            ap4_active,
            atomic_gas_used,
            atomic_batch,
            tx_root,
            receipt_root,
            bloom,
        } = args;

        let spec = self.evm_config.chain_spec();
        let phase = spec.fork_at(ctx.timestamp);
        let ext_data = ext_data_of(atomic_batch)?;
        let ext_data_hash = if ext_data.is_empty() {
            empty_ext_data_hash()
        } else {
            keccak256(&ext_data)
        };

        // coreth `consensus/dummy/consensus.go:334-352` — the dummy engine
        // finalizes the header by prepending the ACP-176/AP3 extra prefix and, at
        // Granite, stamping the millisecond timestamp + ACP-226 min-delay-excess.
        // A Granite header reports `TimeMilliseconds`; earlier headers derive it
        // from `Time × 1000` (so `time_ms_field` is `None` there). We build with
        // no `GasTarget`/`MinDelayTarget` override, so both desired values are
        // `nil` (coreth `vm.go:535-543`).
        let time_ms_field = spec.is_granite(ctx.timestamp).then_some(ctx.timestamp_ms);
        let mut extra = crate::feerules::extra_prefix(
            spec,
            parent,
            ctx.timestamp,
            time_ms_field,
            gas_used,
            atomic_gas_used,
            None,
        )?;
        // coreth `core/evm.go:187` (`SetPredicateBytesInExtra`) — post-Durango,
        // the header's `Extra` carries a fixed-offset warp-predicate-results
        // suffix after the ACP-176/window prefix (see
        // [`EMPTY_BLOCK_PREDICATE_RESULTS`]).
        if spec.is_durango(ctx.timestamp) {
            extra.extend_from_slice(&EMPTY_BLOCK_PREDICATE_RESULTS);
        }
        let min_delay_excess =
            crate::feerules::min_delay_excess_of(spec, parent, ctx.timestamp, None)?;

        // AP3+ carries an explicit base fee; pre-AP3 leaves it absent.
        let base_fee_field = (phase >= AvaPhase::ApricotPhase3).then(|| U256::from(base_fee));
        // AP4+ carries the ext-data gas used + block gas cost tail.
        let (ext_data_gas_used, block_gas_cost_field) = if ap4_active {
            (
                Some(U256::from(atomic_gas_used)),
                Some(U256::from(block_gas_cost_value)),
            )
        } else {
            (None, None)
        };
        // coreth `miner/worker.go:186-197` — the Cancun tail (== Etna on
        // Avalanche).
        let (blob_gas_used, excess_blob_gas, parent_beacon_root) = cancun_tail(phase);

        Ok(AvaHeader {
            parent_hash,
            uncle_hash: EMPTY_OMMER_ROOT_HASH,
            // coreth `plugin/evm/vm.go:565` — the miner's etherbase is pinned
            // to the blackhole address (rewards disabled/burned at the
            // consensus layer, not a per-block suggestion).
            coinbase: BLACKHOLE_ADDRESS,
            state_root,
            // coreth `customtypes/block_ext.go:189` — `NewBlockWithExtData`
            // derives TxHash/ReceiptHash/Bloom from the body at assembly.
            tx_root,
            receipt_root,
            bloom: Bytes::copy_from_slice(bloom.as_slice()),
            // coreth `consensus/dummy/consensus.go:233-235` (`Prepare`) —
            // every header's difficulty is stamped to exactly 1.
            difficulty: U256::from(1),
            number: parent.number.saturating_add(1),
            gas_limit,
            gas_used,
            time: ctx.timestamp,
            // coreth `customheader/extra.go:30` (`ExtraPrefix`) — the exact
            // Fortuna+ 24-byte ACP-176 fee state (or AP3 window / empty pre-AP3)
            // Go's dummy-engine `VerifyExtraPrefix` checks byte-for-byte, plus
            // the Durango+ predicate-results suffix appended above.
            extra: Bytes::from(extra),
            mix_digest: B256::ZERO,
            nonce: [0u8; 8],
            ext_data_hash,
            base_fee: base_fee_field,
            ext_data_gas_used,
            block_gas_cost: block_gas_cost_field,
            blob_gas_used,
            excess_blob_gas,
            parent_beacon_root,
            // coreth `consensus/dummy/consensus.go:334-352` — the Granite header
            // tail (millisecond timestamp + ACP-226 min-delay-excess).
            time_milliseconds: time_ms_field,
            min_delay_excess,
        })
    }

    /// Resolves the block-gas-cost regime for the next block from the active
    /// phase (spec 21 §4b): inactive pre-AP4, AP4 step until AP5, AP5 step after,
    /// retired in Granite.
    fn block_gas_cost_params(&self, ctx: &AvaNextBlockCtx) -> BlockGasCostParams {
        let phase = self.evm_config.chain_spec().fork_at(ctx.timestamp);
        let active = phase >= AvaPhase::ApricotPhase4;
        let step = if phase >= AvaPhase::ApricotPhase5 {
            BLOCK_GAS_COST_STEP_AP5
        } else {
            BLOCK_GAS_COST_STEP_AP4
        };
        BlockGasCostParams {
            active,
            step,
            granite: phase >= AvaPhase::Granite,
        }
    }
}

/// The EVM txs packed for a block + the env header they execute under.
struct PackedEvm {
    txs: Vec<RecoveredTx>,
    env_header: Header,
}

/// The Cancun (== Etna on Avalanche) header tail for a block resolved at
/// `phase`: `(blob_gas_used, excess_blob_gas, parent_beacon_root)`. All three
/// are `Some` (clamped to the zero/empty value — the C-Chain has no real blobs
/// or beacon chain) at Etna+ and `None` pre-Etna (coreth `miner/worker.go:186-197`;
/// `EvmBlock::syntactic_verify` in `block.rs` enforces exactly this shape on
/// the verify path). Shared by [`BlockBuilderDriver::pack_evm_txs`] (the
/// execution env header — reth's beacon-root system call requires this even
/// before the coreth `AvaHeader` is assembled) and
/// [`BlockBuilderDriver::build_header`] (the assembled header).
fn cancun_tail(phase: AvaPhase) -> (Option<u64>, Option<u64>, Option<B256>) {
    if phase >= AvaPhase::Etna {
        (Some(0), Some(0), Some(B256::ZERO))
    } else {
        (None, None, None)
    }
}

/// Grouped arguments for [`BlockBuilderDriver::build_header`] (the coreth header
/// has many fork-gated fields; a struct keeps the call site readable).
struct BuildHeaderArgs<'a> {
    parent: &'a AvaHeader,
    parent_hash: B256,
    ctx: &'a AvaNextBlockCtx,
    state_root: B256,
    base_fee: u64,
    gas_limit: u64,
    gas_used: u64,
    block_gas_cost: u64,
    ap4_active: bool,
    atomic_gas_used: u64,
    atomic_batch: &'a [SignedAtomicTx],
    /// The ordered-trie transactions root over the block body (coreth
    /// `customtypes/block_ext.go:189`).
    tx_root: B256,
    /// The typed-receipt trie root over the batch's executed receipts (the
    /// SAME `EthReceipt::calculate_receipt_root_no_memo` mechanism
    /// `ava-saevm-exec::driver.rs:269` uses).
    receipt_root: B256,
    /// The OR-fold of every receipt's logs bloom.
    bloom: Bloom,
}

/// The unsigned bodies of the atomic batch (the form [`AtomicStateHook`] and the
/// block `atomic_txs` field carry).
fn unsigned_of(batch: &[SignedAtomicTx]) -> Vec<AtomicTx> {
    batch.iter().map(|tx| tx.unsigned.clone()).collect()
}

/// The signed-byte length of each atomic tx (parallel to `batch`), for the
/// per-tx atomic-gas accumulation ([`AtomicStateHook::batch_gas`]).
fn atomic_tx_lens(batch: &[SignedAtomicTx]) -> Vec<u64> {
    batch
        .iter()
        .map(|tx| u64::try_from(tx.bytes().len()).unwrap_or(u64::MAX))
        .collect()
}

/// Encodes the atomic batch into the block `ExtData` bytes (the AP5 batch
/// encoding `atomic.Codec.Marshal(0, []*Tx)` over the **signed** txs; empty when
/// there are no atomic txs, §6.2). The pre-image of `ext_data_hash` and the exact
/// inverse of `block::extract_atomic_txs` (which `unmarshal`s a `Vec<Tx>`).
///
/// # Errors
/// Returns [`Error::NilTx`] if the codec fails to marshal the batch.
fn ext_data_of(batch: &[SignedAtomicTx]) -> Result<Vec<u8>> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }
    atomic_codec()
        .marshal(CODEC_VERSION, &batch.to_vec())
        .map_err(|_| Error::NilTx)
}

/// Narrows a `U256` header field to `u64` (block gas cost / ext-data gas are
/// bounded well below `u64::MAX`; an out-of-range value saturates).
fn u256_to_u64(v: U256) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// Projects the coreth parent [`AvaHeader`] onto the fee-bearing reth [`Header`]
/// fields [`AvaEvmConfig::next_evm_env`] consumes (number, timestamp, gas limit,
/// base fee, parent hash). The coreth-specific extras (`ext_data_hash`, the AP4
/// tail, …) are not part of reth's next-env derivation.
///
/// # Errors
/// Returns [`Error::NilBaseFee`] if the parent's base fee exceeds `u64::MAX`
/// (a malformed header — C-Chain base fees fit in `u64`).
fn parent_eth_header(parent: &AvaHeader) -> Result<Header> {
    let base_fee_per_gas = match parent.base_fee {
        Some(bf) => Some(u64::try_from(bf).map_err(|_| Error::NilBaseFee)?),
        None => None,
    };
    Ok(Header {
        parent_hash: parent.parent_hash,
        number: parent.number,
        timestamp: parent.time,
        gas_limit: parent.gas_limit,
        gas_used: parent.gas_used,
        base_fee_per_gas,
        beneficiary: parent.coinbase,
        ..Default::default()
    })
}
