// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-evm` reuse seam ([`EvmDriver`]) and the per-block hook seam
//! ([`ExecHooks`]) the execute step drives (specs/00 §11.1.5, specs/11 §6.1).
//!
//! [`EvmDriver`] is the "one EVM, two drivers" boundary: the production
//! [`AvaEvmDriver`] reuses `ava-evm`'s revm + Firewood path; tests can supply a
//! lightweight fake so [`execute_step`](crate::execute_step) is exercised
//! without a live revm. [`ExecHooks`] is the SAE block-lifecycle hook seam
//! (`before/after_executing_block`, `end_of_block_ops`, `gas_config_after`,
//! `block_time`); the production C-Chain bodies land in M7.21, so the no-op
//! [`NoopExecHooks`] is provided here for the pure-EVM path.

use std::sync::Arc;

use ava_evm::{AvaEvmConfig, FirewoodStateProvider, NoopPreHook};
use ava_evm_reth::{B256, EthReceipt, ExternalConsensusExecutor};
use ava_saevm_blocks::Block;
use ava_saevm_gastime::{GasPriceConfig, GasTime};
use ava_saevm_hook::Op;
use ava_saevm_types::SealedHeader;
use ava_vm::components::gas::{Gas, Price};

use crate::error::{Error, Result};

/// One executed transaction's receipt, published to the
/// [`ReceiptSink`](crate::ReceiptSink) as it is produced (specs/11 §6.1 step 6).
///
/// A thin, lifecycle-decoupled subset of the Go `saexec.Receipt`: the tx hash,
/// the gas it consumed, the effective gas price, and whether it reverted (a
/// revert is **normal** and still consumes gas). The full receipt-field
/// derivation (logs/block-hash fixups, `EffectiveGasPrice`) is wired with the
/// real receipt type in M7.15/M7.21.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxReceipt {
    /// The transaction hash.
    pub tx_hash: B256,
    /// Gas consumed by the transaction (a reverted tx still consumes gas).
    pub gas_used: u64,
    /// The effective gas price. Currently the realised `base_fee`; the per-tx
    /// priority-tip fixup (`base_fee + tip`) lands with the real receipt type in
    /// M7.15/M7.21.
    pub effective_gas_price: Price,
    /// Whether the transaction reverted. A revert is normal; an *errored* tx is
    /// fatal and never produces a [`TxReceipt`] (it aborts the block).
    pub reverted: bool,
}

/// The outcome of executing one block's transactions + end-of-block ops through
/// the EVM reuse seam (specs/11 §6.1 steps 6–7, 10).
///
/// Decoupled from any block lifecycle (mirrors `ava_evm_reth::ExecOutcome`) so
/// the pure [`execute_step`](crate::execute_step) can thread it into the
/// commit. The `post_state_root` is the Firewood **pre-commit** root for the
/// proposed bundle (`propose_from_bundle`); committing it is step 10.
pub struct BlockOutcome {
    /// Per-transaction receipts, in execution order.
    pub receipts: Vec<TxReceipt>,
    /// Total gas consumed by the block (txs + end-of-block ops).
    pub gas_used: u64,
    /// The proposed (pre-commit) post-execution state root.
    pub post_state_root: B256,
    /// The receipts-trie root **derived from the receipts this execution
    /// produced** (`derive_sha(receipts)`, via reth's
    /// `calculate_receipt_root_no_memo`). This is the value persisted into the
    /// consensus-critical [`ExecutionResults::receipt_root`](ava_saevm_types::ExecutionResults);
    /// it is NOT the executed eth header's `ReceiptHash`, which under SAE is the
    /// settled ancestor's reinterpreted results (specs/11 §4.1, §10 inv 10).
    pub receipt_root: B256,
}

/// The `ava-evm` reuse seam: open the parent state, execute the block's ordered
/// transactions + end-of-block ops, and propose the post-state root — without
/// committing (specs/00 §11.1.5).
///
/// The execute step owns the *policy* (clock, base-fee bound, D→M→I→X commit
/// order); the driver owns the *EVM mechanism*. Implementors MUST be pure with
/// respect to `(parent_root, header, base_fee, txs, ops)` — no wall-clock, no
/// unsorted iteration (specs/00 §6.1).
pub trait EvmDriver {
    /// Execute `block`'s transactions against the parent post-execution state at
    /// `parent_root`, at the consensus-agreed `base_fee`, then apply
    /// `end_of_block_ops`, and propose the resulting post-state root.
    ///
    /// Returns the receipts + gas + proposed roots (uncommitted). The proposed
    /// root is committed by the execute step's step 10.
    ///
    /// # Errors
    /// [`Error::Fatal`] if a transaction *errored* (vs. reverted), an op could
    /// not be applied, or the parent state could not be opened/proposed.
    fn execute_block(
        &self,
        block: &Block,
        parent_root: B256,
        base_fee: Price,
        end_of_block_ops: &[Op],
    ) -> Result<BlockOutcome>;
}

/// The SAE per-block lifecycle hook seam consumed by the execute step
/// (specs/11 §6.1, Go `hook.Points`).
///
/// A narrowed, object-safe-friendly subset of [`ava_saevm_hook::Points`]
/// covering exactly the methods the execute step calls. The full
/// [`Points`](ava_saevm_hook::Points) trait carries libevm-typed associated
/// types (`Rules`, `Receipts`, state handles) whose bodies are deferred to
/// M7.21; this seam keeps the execute step independent of that wiring while the
/// C-Chain hooks are built.
pub trait ExecHooks {
    /// The exact block time `(unix_seconds, sub-second nanos)` for `header`, as
    /// recorded in `BlockBuilder::build_header` (Go `Points.BlockTime`).
    fn block_time(&self, header: &SealedHeader) -> (u64, u32);

    /// The gas target + config that go into effect immediately after `header`
    /// (Go `Points.GasConfigAfter`). Used for the after-block clock update.
    fn gas_config_after(&self, header: &SealedHeader) -> (Gas, GasPriceConfig);

    /// Operations to perform after the regular EVM transactions
    /// (Go `Points.EndOfBlockOps`).
    ///
    /// # Errors
    /// Any error here is **fatal** (Go wraps it in `errFatal`): the block's
    /// agreed end-of-block ops MUST apply.
    fn end_of_block_ops(&self, block: &Block) -> Result<Vec<Op>>;
}

/// A no-op [`ExecHooks`] for the pure-EVM path: zero end-of-block ops, a unit
/// gas config, the header's own timestamp as the block time. Sufficient for the
/// M7.14 execute-step integration and tests; the C-Chain hook bodies land in
/// M7.21.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopExecHooks {
    /// The gas target to report from [`ExecHooks::gas_config_after`].
    pub target: u64,
}

impl ExecHooks for NoopExecHooks {
    fn block_time(&self, header: &SealedHeader) -> (u64, u32) {
        (header.timestamp, 0)
    }

    fn gas_config_after(&self, _header: &SealedHeader) -> (Gas, GasPriceConfig) {
        (Gas(self.target.max(1)), GasPriceConfig::default())
    }

    fn end_of_block_ops(&self, _block: &Block) -> Result<Vec<Op>> {
        Ok(Vec::new())
    }
}

/// The production [`EvmDriver`]: reuses `ava-evm`'s revm + Firewood execution
/// path (the "one EVM, two drivers" async driver, specs/00 §11.1.5).
///
/// Wraps an [`AvaEvmConfig`] (the reth `ConfigureEvm` + chain spec) and the
/// shared [`FirewoodStateProvider`] (the execution trie). [`execute_block`]
/// opens the parent view, drives [`AvaEvmConfig`]'s
/// [`execute_batch`](ava_evm_reth::ExternalConsensusExecutor::execute_batch) over
/// the recovered transactions, and proposes the resulting bundle via
/// [`FirewoodStateProvider::propose_from_bundle`].
///
/// [`execute_block`]: EvmDriver::execute_block
pub struct AvaEvmDriver {
    config: AvaEvmConfig,
    state: Arc<FirewoodStateProvider>,
}

impl AvaEvmDriver {
    /// Builds the driver over an `ava-evm` config and Firewood state provider.
    #[must_use]
    pub fn new(config: AvaEvmConfig, state: Arc<FirewoodStateProvider>) -> Self {
        Self { config, state }
    }
}

impl EvmDriver for AvaEvmDriver {
    fn execute_block(
        &self,
        block: &Block,
        parent_root: B256,
        base_fee: Price,
        end_of_block_ops: &[Op],
    ) -> Result<BlockOutcome> {
        use ava_evm_reth::{SignerRecoverable, StateBuilder, StateProviderDatabase};

        let eth = block.eth_block();

        // Recover the block's transaction senders (the unit `execute_batch`
        // consumes). An unrecoverable signature is fatal — consensus agreed
        // this block, so its txs MUST recover. Mirrors `EvmBlock::recover_senders`.
        let recovered = eth
            .body()
            .transactions
            .iter()
            .map(|tx| {
                tx.clone()
                    .try_into_recovered()
                    .map_err(|_| Error::Fatal("transaction sender recovery failed".to_owned()))
            })
            .collect::<Result<Vec<_>>>()?;

        // Open the parent post-execution state and build the `AvaState` revm
        // overlay over it (the verify-path shape, specs/11 §6.1 step 3 /
        // specs/10 §3.2; mirrors `EvmBlock::verify`).
        let view = self
            .state
            .history_by_state_root(parent_root)
            .map_err(|_| Error::StateDb(ava_saevm_db::Error::NoRevision(parent_root)))?;
        let mut overlay = StateBuilder::new()
            .with_database(StateProviderDatabase::new(view))
            .with_bundle_update()
            .build();

        // The header drives the per-block execution context; override its base
        // fee with the consensus-agreed worst-case-bounded value (specs/11
        // §6.1 step 5: "reduce gas price from the worst-case value"). The
        // changed header reshapes the env the block executes under.
        let mut header = eth.header().clone();
        header.base_fee_per_gas = Some(base_fee.0);
        let env = self.config.evm_env_for_header(&header);

        let outcome = self
            .config
            .execute_batch(env, &mut overlay, &NoopPreHook, &recovered)
            .map_err(|e| {
                // A reth block-execution error here means a tx *errored* (not
                // reverted) — fatal (Go `errFatal`). Reverts are reported as
                // unsuccessful receipts inside `result`, never as this error.
                Error::Fatal(format!("transaction execution errored (not reverted): {e}"))
            })?;

        // Per-tx receipts. A reth `EthReceipt` carries `success: bool`; a `false`
        // here is a REVERT (normal, gas still consumed), not an error (which
        // would have surfaced above). Cumulative gas is differenced into per-tx
        // gas.
        let mut receipts = Vec::with_capacity(outcome.result.receipts.len());
        let mut prev_cumulative: u64 = 0;
        for (tx, receipt) in recovered.iter().zip(outcome.result.receipts.iter()) {
            let cumulative = receipt.cumulative_gas_used;
            let gas_used = cumulative.saturating_sub(prev_cumulative);
            prev_cumulative = cumulative;
            receipts.push(TxReceipt {
                tx_hash: *tx.tx_hash(),
                gas_used,
                effective_gas_price: base_fee,
                reverted: !receipt.success,
            });
        }

        // End-of-block ops (mint/burn) modify the post-tx state. The faithful
        // application onto the revm overlay before the bundle is materialised
        // requires the revm-backed `StateMut` adapter; for the M7.14 pure-EVM
        // path the hook returns no ops, so this is a checked no-op. Applying
        // non-empty ops onto the same overlay is wired with the C-Chain hooks.
        // TODO(M7.21): revm-backed `StateMut` op application onto the overlay.
        if !end_of_block_ops.is_empty() {
            return Err(Error::Fatal(
                "end-of-block op application onto the revm overlay is not wired (M7.21)".to_owned(),
            ));
        }

        // Derive the receipts-trie root from the receipts THIS execution
        // produced (specs/11 §10 invariant 10: `receipt_root == derive_sha(receipts)`).
        // The header's `ReceiptHash` under SAE is the *settled ancestor's*
        // reinterpreted results, NOT this block's executed receipts — so it must
        // not be reused here. `calculate_receipt_root_no_memo` is the same
        // bloom-recomputing ordered-trie helper `ava-evm` / reth use over the
        // reth `EthReceipt` type the executor produces.
        let receipt_root = EthReceipt::calculate_receipt_root_no_memo(&outcome.result.receipts);

        // Propose the post-state root (pre-commit; stashes by root). Step 10
        // commits it.
        let post_state_root = self
            .state
            .propose_from_bundle(&outcome.bundle)
            .map_err(|e| Error::Fatal(format!("propose post-state bundle: {e}")))?;

        Ok(BlockOutcome {
            receipts,
            gas_used: outcome.result.gas_used,
            post_state_root,
            receipt_root,
        })
    }
}

/// Rebuilds a full [`GasTime`] clock from a parent block's persisted
/// proxy-clock instant plus the hook's post-parent gas config (specs/11 §6.1
/// step 2; the saedb-opener-driven clock reconstruction deferred from M7.13).
///
/// The Rust `ExecutionResults` persists only the `Time<u64>` proxy instant (not
/// the full ACP-176 `target`/`excess`), so the parent's clock is reconstructed
/// from `(unix_seconds, fraction)` of the persisted instant, with the
/// `target`/`excess`/`config` supplied by the hook + the held-clock continuity
/// (mirrors `hook::Settled::settled_gas_time`). The executor caches the parent's
/// full [`GasTime`] across blocks for exact continuity; this helper is the
/// recovery/standalone fallback.
#[must_use]
pub fn rebuild_gas_clock(
    parent_instant: &ava_saevm_proxytime::Time<u64>,
    target: Gas,
    excess: Gas,
    config: GasPriceConfig,
) -> GasTime {
    GasTime::from_settled(
        parent_instant.unix_seconds(),
        parent_instant.fraction().numerator,
        target.0,
        excess.0,
        config,
    )
}
