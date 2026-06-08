// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `AvaEvmConfig` (a [`ConfigureEvm`] wrapper) + the
//! [`ExternalConsensusExecutor::execute_batch`] driving over a bare reth
//! [`BlockExecutor`] (spec 10 §7/§8/§17.1/§17.4). The per-fork fee override
//! (`next_evm_env`) lands in M6.13; this module (M6.6) delivers the
//! executor-driving entry point — the cheapest differential oracle (02 §10.5).
//!
//! ## Design (spec 10 §17.1 — "reth as a library", G6)
//!
//! Rather than re-deriving the whole [`ConfigureEvm`] trait by hand,
//! `AvaEvmConfig` wraps reth's ready-made `EthEvmConfig` parameterised on
//! [`AvaExecutorSpec`] — a thin adapter over [`AvaChainSpec`] that supplies the
//! two super-traits (`EthExecutorSpec` + `Hardforks`) `EthEvmConfig`'s
//! `ConfigureEvm` bound requires, and that pins the **Avalanche fork-activation
//! semantics reth's executor needs but the timestamp-keyed `AvaChainSpec`
//! schedule does not encode**:
//!
//! - Avalanche is **always post-merge** (no PoW block reward): `AvaChainSpec`
//!   reports `final_paris_total_difficulty == 0` and (since M6.8) activates
//!   Paris + every pre-merge Ethereum fork at `ForkCondition::Block(0)`, so
//!   reth's `base_block_reward` (keyed on `is_paris_active_at_block`) resolves
//!   post-merge from block 0 and applies **no** block reward, matching coreth.
//!   `AvaExecutorSpec` is now a thin pass-through that delegates the fork view
//!   straight to [`AvaChainSpec`] (the M6.6 force-activation override was lifted
//!   once the chainspec keyed those forks by block — M6.8).
//!
//! `execute_batch` then drives the inner config's bare [`BlockExecutor`]
//! directly: build the per-block `EthBlockExecutionCtx` from the env header, run
//! the atomic [`PreExecutionHook`], `apply_pre_execution_changes` →
//! `execute_transaction` loop → `finish`, then merge/take the revm `BundleState`
//! the caller turns into a Firewood proposal (§17.2). No node, no engine, no
//! fork choice (§17.1).

use std::borrow::Cow;
use std::fmt::Display;
use std::sync::Arc;

use ava_evm_reth::{
    Address, AvaEvmEnv, AvaEvmError, B256, BaseFeeParams, BlobParams, BlockExecutor,
    BundleRetention, Chain, ConfigureEvm, Database, DepositContract, EthBlockExecutionCtx,
    EthChainSpec, EthEvmConfig, EthExecutorSpec, EthereumHardfork, EthereumHardforks, ExecOutcome,
    ExternalConsensusExecutor, ForkCondition, ForkFilter, ForkFilterKey, ForkHash, ForkId, Genesis,
    Hardfork, Hardforks, Head, Header, NextBlockEnvAttributes, NodeRecord, PreExecutionHook,
    RecoveredTx, State, StateDbError, StateProviderDatabase, U256,
};
use ruint::aliases::U256 as RuintU256;

use crate::chainspec::AvaChainSpec;
use crate::error::Error;
use crate::feerules::acp176::Acp176State;
use crate::feerules::window::Window;
use crate::feerules::{base_fee, gas_limit};
use crate::precompile::registry::{AvaCtxExt, AvaPrecompiles, PrecompileRegistry};
use crate::state::FirewoodStateView;

/// The Avalanche-specific dynamic-fee state carried from the parent block into
/// the next-block build/verify context (spec 10 §17.3, spec 21 §7).
///
/// The active fork decides which variant is meaningful; the builder/verifier
/// extracts it from the parent header's extra-data (the AP3 rolling window +
/// parent base fee, or the ACP-176 24-byte fee-state blob — M6.7) and threads it
/// here so [`feerules::base_fee`] is a pure function of `(spec, parent, ctx)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AvaFeeState {
    /// AP3..Fortuna rolling-window regime: the parent's gas window + base fee.
    Window {
        /// `feeWindow` parsed from `parent.Extra` (empty for genesis/first-AP3).
        window: Window,
        /// `parent.BaseFee` (wei).
        base_fee: RuintU256,
    },
    /// Fortuna+ ACP-176 regime: the parent's 24-byte fee state. `gas_price()`
    /// yields the next-block base fee directly.
    Acp176(Acp176State),
}

impl Default for AvaFeeState {
    fn default() -> Self {
        // Genesis / pre-AP3 default: an empty window with a zero base fee. The
        // pre-AP3 base_fee dispatch returns `NilBaseFee` regardless of this.
        AvaFeeState::Window {
            window: Window::default(),
            base_fee: RuintU256::ZERO,
        }
    }
}

/// The next-block build/verify context — `ConfigureEvm::NextBlockEnvCtx` in the
/// spec §17.3 design. Carries the Avalanche-specific inputs reth's
/// `next_evm_env` does not model: the sub-second (ACP-226) timestamp, the
/// P-Chain height (warp predicate ctx, §17.5), the atomic gas budget, and the
/// parent dynamic-fee state ([`AvaFeeState`]).
///
/// **Relationship to `AvaBlockCtx`** (`precompile::registry::AvaBlockCtx`, M6.21):
/// distinct concerns. `AvaNextBlockCtx` is the **build/fee** context consumed by
/// [`AvaEvmConfig::next_evm_env`] to derive `block_env.{basefee, gas_limit}`;
/// `AvaBlockCtx` is the **revm chain-slot extension** (G10) the precompile
/// handler reads at call time. They are not interchangeable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AvaNextBlockCtx {
    /// The timestamp (unix seconds) of the next block.
    pub timestamp: u64,
    /// The sub-second (ACP-226) timestamp in milliseconds.
    pub timestamp_ms: u64,
    /// The suggested fee recipient (coinbase) for the next block.
    pub suggested_fee_recipient: Address,
    /// An optional builder-supplied gas-limit override (else the phase default).
    pub gas_limit_hint: Option<u64>,
    /// The P-Chain height pinned for this block (warp predicate ctx, §17.5).
    pub pchain_height: u64,
    /// The parent dynamic-fee state (window or ACP-176), spec 21 §7.
    pub parent_fee_state: AvaFeeState,
    /// The maximum total atomic-tx gas the next block may include (the budget
    /// [`crate::atomic::mempool::AtomicMempool::next_batch`] packs against,
    /// `ap5.AtomicGasLimit = 100_000`).
    pub atomic_gas_limit: u64,
}

impl Default for AvaNextBlockCtx {
    fn default() -> Self {
        Self {
            timestamp: 0,
            timestamp_ms: 0,
            suggested_fee_recipient: Address::ZERO,
            gas_limit_hint: None,
            pchain_height: 0,
            parent_fee_state: AvaFeeState::default(),
            atomic_gas_limit: 0,
        }
    }
}

impl AvaNextBlockCtx {
    /// Builds a context with the given atomic gas budget (the minimal form the
    /// atomic mempool needs; the builder fills the remaining fields). Preserves
    /// the M6.16 ergonomic constructor while the full type lives here (M6.13).
    #[must_use]
    pub fn with_atomic_gas_limit(atomic_gas_limit: u64) -> Self {
        Self {
            atomic_gas_limit,
            ..Self::default()
        }
    }
}

/// A pre-execution hook that does nothing — the reexecute / pure-EVM path, where
/// there is no atomic Import/Export to apply. The atomic-tx hook lands in M6.16.
pub struct NoopPreHook;

impl PreExecutionHook for NoopPreHook {
    fn apply(&self, _db: &mut dyn Database<Error = StateDbError>) -> Result<(), AvaEvmError> {
        Ok(())
    }
}

/// The revm `State<DB>` overlay `execute_batch` runs against: a Firewood-ethhash
/// read view wrapped into a revm `Database` by reth's `StateProviderDatabase`.
pub type AvaState = State<StateProviderDatabase<FirewoodStateView>>;

/// Adapter over [`AvaChainSpec`] that satisfies the `EthEvmConfig` `ConfigureEvm`
/// bound (`EthChainSpec + EthExecutorSpec + Hardforks`). Since M6.8 it is a thin
/// pass-through: the Avalanche post-merge fork semantics reth's executor needs
/// (Paris + pre-merge forks active at `Block(0)`, `final_paris_total_difficulty
/// == 0`) are pinned in [`AvaChainSpec`] itself, so this adapter just forwards
/// every method (it exists only to bridge the `EthExecutorSpec` super-trait,
/// which `AvaChainSpec` does not implement directly).
#[derive(Clone, Debug)]
pub struct AvaExecutorSpec(Arc<AvaChainSpec>);

impl EthChainSpec for AvaExecutorSpec {
    type Header = Header;

    fn chain(&self) -> Chain {
        EthChainSpec::chain(self.0.as_ref())
    }

    fn base_fee_params_at_timestamp(&self, timestamp: u64) -> BaseFeeParams {
        self.0.base_fee_params_at_timestamp(timestamp)
    }

    fn blob_params_at_timestamp(&self, timestamp: u64) -> Option<BlobParams> {
        self.0.blob_params_at_timestamp(timestamp)
    }

    fn deposit_contract(&self) -> Option<&DepositContract> {
        self.0.deposit_contract()
    }

    fn genesis_hash(&self) -> B256 {
        self.0.genesis_hash()
    }

    fn prune_delete_limit(&self) -> usize {
        self.0.prune_delete_limit()
    }

    fn display_hardforks(&self) -> Box<dyn Display> {
        self.0.display_hardforks()
    }

    fn genesis_header(&self) -> &Header {
        self.0.genesis_header()
    }

    fn genesis(&self) -> &Genesis {
        self.0.genesis()
    }

    fn bootnodes(&self) -> Option<Vec<NodeRecord>> {
        self.0.bootnodes()
    }

    fn final_paris_total_difficulty(&self) -> Option<U256> {
        self.0.final_paris_total_difficulty()
    }
}

impl EthereumHardforks for AvaExecutorSpec {
    fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> ForkCondition {
        // Delegate straight to the chain spec: M6.8 keys Paris + pre-merge forks
        // by `Block(0)` and Berlin/London/Shanghai/Cancun by Avalanche-phase
        // timestamp, so reth's block-reward / merge checks resolve correctly.
        self.0.ethereum_fork_activation(fork)
    }
}

impl EthExecutorSpec for AvaExecutorSpec {
    fn deposit_contract_address(&self) -> Option<Address> {
        None
    }
}

impl Hardforks for AvaExecutorSpec {
    fn fork<H: Hardfork>(&self, fork: H) -> ForkCondition {
        self.0.hardforks().fork(fork)
    }

    fn forks_iter(&self) -> impl Iterator<Item = (&dyn Hardfork, ForkCondition)> {
        self.0.hardforks().forks_iter()
    }

    fn fork_id(&self, head: &Head) -> ForkId {
        // EIP-6122 fork hash: start from the genesis hash, fold in each active
        // block/timestamp fork in order, stop at the first inactive one
        // (recorded as `next`). Mirrors reth `ChainSpec::fork_id` over our
        // public `forks_iter`. Used only on the p2p eth handshake path, never
        // during execution.
        let mut forkhash = ForkHash::from(self.genesis_hash());
        let mut current_applied = 0u64;

        for (_, cond) in self.forks_iter() {
            if let ForkCondition::Block(block) = cond {
                if head.number >= block {
                    if block != current_applied {
                        forkhash += block;
                        current_applied = block;
                    }
                } else {
                    return ForkId {
                        hash: forkhash,
                        next: block,
                    };
                }
            }
        }

        let genesis_timestamp = self.genesis().timestamp;
        for timestamp in self
            .forks_iter()
            .filter_map(|(_, cond)| cond.as_timestamp().filter(|t| *t > genesis_timestamp))
        {
            if head.timestamp >= timestamp {
                if timestamp != current_applied {
                    forkhash += timestamp;
                    current_applied = timestamp;
                }
            } else {
                return ForkId {
                    hash: forkhash,
                    next: timestamp,
                };
            }
        }

        ForkId {
            hash: forkhash,
            next: 0,
        }
    }

    fn latest_fork_id(&self) -> ForkId {
        self.fork_id(&Head {
            number: u64::MAX,
            timestamp: u64::MAX,
            ..Default::default()
        })
    }

    fn fork_filter(&self, head: Head) -> ForkFilter {
        let forks = self
            .forks_iter()
            .filter_map(|(_, condition)| match condition {
                ForkCondition::Block(block) => Some(ForkFilterKey::Block(block)),
                ForkCondition::Timestamp(time) => Some(ForkFilterKey::Time(time)),
                _ => None,
            });
        ForkFilter::new(head, self.genesis_hash(), self.genesis().timestamp, forks)
    }
}

/// The Avalanche C-Chain EVM configuration (spec 10 §7/§8/§17.1).
///
/// Wraps reth's `EthEvmConfig` specialised to [`AvaExecutorSpec`], reusing reth's
/// `ConfigureEvm` machinery (G6) while sourcing the fork schedule + spec id from
/// the Avalanche network-upgrade schedule (G7). M6.13 layers the per-fork fee
/// override on top via `next_evm_env`.
#[derive(Clone, Debug)]
pub struct AvaEvmConfig {
    /// reth's Ethereum `ConfigureEvm`, parameterised on the Avalanche exec spec.
    inner: EthEvmConfig<AvaExecutorSpec>,
    /// The Avalanche chain spec (fork schedule) — owned here so the precompile
    /// height-gating (`precompiles_for_header`) can read the per-block timestamp.
    chain_spec: Arc<AvaChainSpec>,
    /// The Avalanche stateful-precompile registry (G4, §8). `for_height` reads it
    /// to build the activated `warm` set per block (M6.21). Defaults to empty;
    /// M6.22 registers the warp/allowlist/feemanager/… modules from genesis +
    /// upgrade config (§8.3) via [`AvaEvmConfig::with_precompiles`].
    precompiles: Arc<PrecompileRegistry>,
}

impl AvaEvmConfig {
    /// Builds the config from an [`AvaChainSpec`], with an empty precompile
    /// registry (no Avalanche stateful precompiles active). M6.22 supplies the
    /// populated registry via [`AvaEvmConfig::with_precompiles`].
    #[must_use]
    pub fn new(chain_spec: AvaChainSpec) -> Self {
        let chain_spec = Arc::new(chain_spec);
        let spec = AvaExecutorSpec(chain_spec.clone());
        Self {
            inner: EthEvmConfig::new(Arc::new(spec)),
            chain_spec,
            precompiles: Arc::new(PrecompileRegistry::new()),
        }
    }

    /// Returns a copy of this config with the given Avalanche stateful-precompile
    /// registry installed (G4, §8). The integration seam M6.22 uses to register
    /// the warp/allowlist/feemanager/nativeminter/rewardmanager modules.
    #[must_use]
    pub fn with_precompiles(mut self, precompiles: Arc<PrecompileRegistry>) -> Self {
        self.precompiles = precompiles;
        self
    }

    /// Builds the Avalanche [`AvaPrecompiles`] revm precompile provider for the
    /// block described by `header`: the activated `warm` set is the registry
    /// modules whose upgrade timestamp is `<= header.timestamp` (G4, §8.3, M6.21).
    ///
    /// This is the integration seam (`AvaBlockExecutorFactory::create_executor` in
    /// §17.5): a custom `EvmFactory` (M6.22) installs this provider — together
    /// with the [`AvaCtxExt`] returned by [`AvaEvmConfig::ctx_ext_for_header`] on
    /// the revm context `Chain` slot (G10) — into the revm handler.
    #[must_use]
    pub fn precompiles_for_header(&self, header: &Header) -> AvaPrecompiles {
        AvaPrecompiles::for_height(self.precompiles.clone(), header.timestamp)
    }

    /// Builds the revm context extension ([`AvaCtxExt`], G10/§17.5) for the block
    /// described by `header`. M6.21 reserves the fields (empty predicate results);
    /// M6.22's pre-execution predicate pass fills them and threads this onto the
    /// revm context `Chain` slot.
    #[must_use]
    pub fn ctx_ext_for_header(&self, header: &Header) -> AvaCtxExt {
        AvaCtxExt {
            block_ctx: crate::precompile::registry::AvaBlockCtx {
                timestamp: header.timestamp,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The Avalanche chain spec (fork schedule) backing this config.
    #[must_use]
    pub fn chain_spec(&self) -> &Arc<AvaChainSpec> {
        &self.chain_spec
    }

    /// The wrapped reth `ConfigureEvm` (the `EthEvmConfig` driving the executor).
    #[must_use]
    pub fn inner(&self) -> &EthEvmConfig<AvaExecutorSpec> {
        &self.inner
    }

    /// Builds the [`AvaEvmEnv`] for executing a block as described by `header`
    /// (the reexecute / verify path, spec 10 §3.2). The fee override
    /// (`next_evm_env`) is M6.13; here the env is taken straight from the header,
    /// which already carries the Go-computed `basefee` / `gas_limit`.
    #[must_use]
    pub fn evm_env_for_header(&self, header: &Header) -> AvaEvmEnv {
        // `EthEvmConfig::evm_env` is infallible (`Error = Infallible`).
        let evm_env = match ConfigureEvm::evm_env(&self.inner, header) {
            Ok(env) => env,
            Err(never) => match never {},
        };
        AvaEvmEnv {
            evm_env,
            header: header.clone(),
        }
    }

    /// `ConfigureEvm::next_evm_env` override (spec 10 §7.2/§17.3 G2): builds the
    /// [`AvaEvmEnv`] for `parent + 1`, then **overrides** `block_env.basefee` and
    /// `block_env.gas_limit` with the Avalanche per-fork fee rules
    /// ([`feerules::base_fee`]/[`feerules::gas_limit`]) keyed on the phase active
    /// at `ctx.timestamp`.
    ///
    /// reth derives the base fee from EIP-1559 inside its own `next_evm_env`;
    /// Avalanche replaced that mechanism in stages (AP3 window → AP4 block gas
    /// cost → Fortuna/ACP-176). We reuse reth's env construction for everything
    /// else (cfg/spec id, beneficiary, prevrandao, blob params) and only swap the
    /// two fee fields.
    ///
    /// **Pre-AP3 (`FeeRegime::Legacy`):** legacy pricing has no base fee, so
    /// `feerules::base_fee` returns [`Error::NilBaseFee`] (coreth `errNilBaseFee`
    /// parity); we leave `block_env.basefee == 0` (treated as "absent"), matching
    /// coreth's nil-base-fee handling.
    ///
    /// **Base-fee-to-coinbase (M6.6 finding #3):** Avalanche *credits* the AP3+
    /// base fee to the coinbase rather than burning it (revm's London default
    /// **burns** it). That is a base-fee-**recipient** change requiring a custom
    /// revm handler / `EvmFactory`, which is **deferred to M6.22** (the same
    /// live-handler install M6.21 deferred). This method sets the base-fee
    /// **value/schedule** only — see the M6.13 report.
    ///
    /// # Errors
    /// Returns [`Error`] if the fork-dispatch base fee cannot be resolved for a
    /// reason other than the pre-AP3 nil case (e.g. the carried fee-state does
    /// not match the active regime — a builder-wiring bug).
    pub fn next_evm_env(&self, parent: &Header, ctx: &AvaNextBlockCtx) -> Result<AvaEvmEnv, Error> {
        // 1. reth's Ethereum baseline env for `parent + 1` (cfg/spec id, block
        //    number/timestamp, beneficiary, prevrandao, blob params). The inner
        //    `EthEvmConfig::next_evm_env` is infallible (`Error = Infallible`).
        let attrs = NextBlockEnvAttributes {
            timestamp: ctx.timestamp,
            suggested_fee_recipient: ctx.suggested_fee_recipient,
            prev_randao: B256::ZERO,
            // A non-zero placeholder so the baseline env is valid; overridden
            // below by `feerules::gas_limit`.
            gas_limit: ctx.gas_limit_hint.unwrap_or(0),
            parent_beacon_block_root: parent.parent_beacon_block_root,
            withdrawals: None,
            extra_data: Default::default(),
            slot_number: None,
        };
        let mut evm_env = match ConfigureEvm::next_evm_env(&self.inner, parent, &attrs) {
            Ok(env) => env,
            Err(never) => match never {},
        };

        // 2. Override the gas limit with the Avalanche per-fork value.
        evm_env.block_env.gas_limit = gas_limit(&self.chain_spec, parent, ctx)?;

        // 3. Override the base fee. Pre-AP3 (`NilBaseFee`) leaves basefee == 0
        //    (treated as nil, coreth `errNilBaseFee` parity).
        match base_fee(&self.chain_spec, parent, ctx) {
            Ok(bf) => evm_env.block_env.basefee = bf,
            Err(Error::NilBaseFee) => evm_env.block_env.basefee = 0,
            Err(e) => return Err(e),
        }

        // 4. The header the per-block execution context is derived from. The
        //    fee-bearing fields mirror the overridden env so the reexecute path
        //    (`evm_env_for_header`) and build path agree.
        let header = Header {
            number: parent.number.saturating_add(1),
            timestamp: ctx.timestamp,
            parent_hash: B256::ZERO,
            parent_beacon_block_root: parent.parent_beacon_block_root,
            beneficiary: ctx.suggested_fee_recipient,
            gas_limit: evm_env.block_env.gas_limit,
            base_fee_per_gas: Some(evm_env.block_env.basefee),
            ..Default::default()
        };

        Ok(AvaEvmEnv { evm_env, header })
    }
}

impl ExternalConsensusExecutor for AvaEvmConfig {
    type State = AvaState;

    fn execute_batch(
        &self,
        env: AvaEvmEnv,
        parent: &mut Self::State,
        pre_hook: &dyn PreExecutionHook,
        txs: &[RecoveredTx],
    ) -> Result<ExecOutcome, AvaEvmError> {
        let AvaEvmEnv { evm_env, header } = env;

        // 1. Apply atomic Import/Export effects (warp predicate pass, etc.) to the
        //    overlay BEFORE the EVM tx loop (spec 10 §17.4). NoopPreHook on the
        //    pure-EVM reexecute path.
        pre_hook.apply(parent)?;

        // 2. Per-block execution context from the block header (parent hash,
        //    beacon root, withdrawals, extra data). Avalanche blocks carry no
        //    ommers; withdrawals are absent pre-Shanghai and unused on C-Chain.
        let ctx = EthBlockExecutionCtx {
            parent_hash: header.parent_hash,
            parent_beacon_block_root: header.parent_beacon_block_root,
            ommers: &[],
            withdrawals: header.withdrawals_root.map(|_| Cow::Owned(Vec::new())),
            extra_data: header.extra_data.clone(),
            tx_count_hint: Some(txs.len()),
            slot_number: None,
        };

        // 3. Build the bare reth block executor over the parent `State` overlay
        //    and drive it: pre-execution changes -> ordered tx loop -> finish.
        let evm = self.inner.evm_with_env(&mut *parent, evm_env);
        let mut executor = self.inner.create_executor(evm, ctx);

        executor.apply_pre_execution_changes()?;
        for tx in txs {
            executor.execute_transaction(tx)?;
        }
        let result = executor.apply_post_execution_changes()?;

        // 4. Materialise the revm `BundleState` the caller turns into a Firewood
        //    proposal (spec 10 §17.2). Keep reverts so verify/reject can unwind.
        parent.merge_transitions(BundleRetention::Reverts);
        let bundle = parent.take_bundle();

        Ok(ExecOutcome { result, bundle })
    }
}
