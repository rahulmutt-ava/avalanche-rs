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
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ava_evm_reth::{
    Address, AvaEvmEnv, AvaEvmError, B256, BaseFeeParams, BlobParams, BlockEnv, BlockEnvTr,
    BlockExecutor, BundleRetention, Cfg, CfgEnv, Chain, ConfigureEvm, Context, ContextSetters,
    ContextTr, DepositContract, DynPrecompile, EVMError, EthBlockExecutionCtx, EthChainSpec,
    EthEvmConfig, EthEvmContext, EthExecutorSpec, EthFrame, EthInstructions, EthInterpreter,
    EthereumHardfork, EthereumHardforks, EvmEnv, EvmFactory, EvmState, EvmTr, EvmTrError,
    ExecOutcome, ExternalConsensusExecutor, ForkCondition, ForkFilter, ForkFilterKey, ForkHash,
    ForkId, FrameInit, FrameResult, FrameTr, Genesis, HaltReason, Handler, Hardfork, Hardforks,
    Head, Header, Inspector, InspectorEvmTr, InspectorHandler, JournalTr, MainBuilder,
    MainContext, NextBlockEnvAttributes, NoOpInspector, NodeRecord, PreExecutionHook,
    PrecompileId, PrecompileInput, PrecompileResult, PrecompileSpecId, Precompiles,
    PrecompilesMap, RecoveredTx, ResultAndState, RevmEvm, State, StateDb, StateProviderDatabase,
    SystemCallEvm, TxEnv, TxEnvTr, U256, post_execution,
};
use ava_evm_reth::Database as RevmDatabase;
use ruint::aliases::U256 as RuintU256;

use crate::chainspec::{AvaChainSpec, AvaPhase};
use crate::error::Error;
use crate::feerules::acp176::Acp176State;
use crate::feerules::window::Window;
use crate::feerules::{base_fee, gas_limit};
use crate::precompile::registry::{
    AvaBlockCtx, AvaCtxExt, AvaPrecompiles, InternalsStateOps, PrecompileCtx, PrecompileRegistry,
    PredicateResults, interpreter_result_to_precompile_output,
};
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
    fn apply(&self, _db: &mut dyn StateDb) -> Result<(), AvaEvmError> {
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

/// The per-block precompile execution context threaded into the live
/// [`AvaEvmFactory`] (M6.31, G4/G10): the verified warp predicate results
/// (filled by the pre-execution predicate pass,
/// [`crate::precompile::warp::prepare_block_predicates`]) plus the
/// proposervm-pinned P-Chain height. The default (empty predicates, height 0)
/// is the no-warp path — `getVerifiedWarpMessage` then reads every index as
/// invalid, exactly like a tx with no predicates in coreth.
#[derive(Clone, Debug, Default)]
pub struct AvaExecCtx {
    /// Verified warp predicate results keyed by tx index (spec 20 §7.2).
    pub predicates: Arc<PredicateResults>,
    /// The proposervm-pinned P-Chain height for this block (spec 10 §17.5).
    pub pchain_height: u64,
}

/// The Avalanche C-Chain EVM configuration (spec 10 §7/§8/§17.1).
///
/// Wraps reth's `EthEvmConfig` specialised to [`AvaExecutorSpec`] **and the
/// custom [`AvaEvmFactory`]** (M6.31), reusing reth's `ConfigureEvm` machinery
/// (G6) while sourcing the fork schedule + spec id from the Avalanche
/// network-upgrade schedule (G7). The factory installs the Avalanche stateful
/// precompiles into every EVM it creates (G4) and routes execution through the
/// [`AvaHandler`] base-fee-to-coinbase fee rule (spec 21 §7). M6.13 layers the
/// per-fork fee override on top via `next_evm_env`.
#[derive(Clone, Debug)]
pub struct AvaEvmConfig {
    /// reth's Ethereum `ConfigureEvm`, parameterised on the Avalanche exec spec
    /// and the Avalanche `EvmFactory` (with an EMPTY per-block exec ctx — the
    /// `eth_call` / no-predicate path; `execute_batch_with_ctx` builds a
    /// per-block config carrying the real predicate results).
    inner: EthEvmConfig<AvaExecutorSpec, AvaEvmFactory>,
    /// The executor-spec adapter (shared so per-block configs are cheap).
    exec_spec: Arc<AvaExecutorSpec>,
    /// The Avalanche chain spec (fork schedule) — owned here so the precompile
    /// height-gating (`precompiles_for_header`) can read the per-block timestamp.
    chain_spec: Arc<AvaChainSpec>,
    /// The Avalanche stateful-precompile registry (G4, §8). The factory reads it
    /// to build the activated set per block. Defaults to empty; the VM assembly
    /// registers the warp/allowlist/feemanager/… modules from genesis + upgrade
    /// config (§8.3) via [`AvaEvmConfig::with_precompiles`].
    precompiles: Arc<PrecompileRegistry>,
}

impl AvaEvmConfig {
    /// Builds the config from an [`AvaChainSpec`], with an empty precompile
    /// registry (no Avalanche stateful precompiles active). The VM assembly
    /// supplies the populated registry via [`AvaEvmConfig::with_precompiles`].
    #[must_use]
    pub fn new(chain_spec: AvaChainSpec) -> Self {
        let chain_spec = Arc::new(chain_spec);
        let exec_spec = Arc::new(AvaExecutorSpec(chain_spec.clone()));
        let precompiles = Arc::new(PrecompileRegistry::new());
        let factory =
            AvaEvmFactory::new(chain_spec.clone(), precompiles.clone(), AvaExecCtx::default());
        Self {
            inner: EthEvmConfig::new_with_evm_factory(exec_spec.clone(), factory),
            exec_spec,
            chain_spec,
            precompiles,
        }
    }

    /// Returns a copy of this config with the given Avalanche stateful-precompile
    /// registry installed (G4, §8) — the registry the live [`AvaEvmFactory`]
    /// installs (height-gated) into every EVM it creates.
    #[must_use]
    pub fn with_precompiles(mut self, precompiles: Arc<PrecompileRegistry>) -> Self {
        self.precompiles = precompiles;
        let factory = AvaEvmFactory::new(
            self.chain_spec.clone(),
            self.precompiles.clone(),
            AvaExecCtx::default(),
        );
        self.inner = EthEvmConfig::new_with_evm_factory(self.exec_spec.clone(), factory);
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

    /// The wrapped reth `ConfigureEvm` (the `EthEvmConfig` driving the executor,
    /// parameterised on the live [`AvaEvmFactory`] — so `eth_call`-style users
    /// of `evm_with_env` get the Avalanche precompiles + fee rule too, with an
    /// empty per-block predicate ctx).
    #[must_use]
    pub fn inner(&self) -> &EthEvmConfig<AvaExecutorSpec, AvaEvmFactory> {
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

impl AvaEvmConfig {
    /// [`ExternalConsensusExecutor::execute_batch`] with an explicit per-block
    /// precompile execution context (M6.31, spec 10 §6.5/§17.5): the verified
    /// warp predicate results (produced by the pre-execution predicate pass)
    /// and the proposervm-pinned P-Chain height. The factory threads them into
    /// every stateful-precompile call of this batch, keyed by tx index.
    ///
    /// # Errors
    /// Returns [`AvaEvmError`] if the pre-hook or any transaction fails.
    pub fn execute_batch_with_ctx(
        &self,
        env: AvaEvmEnv,
        parent: &mut AvaState,
        pre_hook: &dyn PreExecutionHook,
        txs: &[RecoveredTx],
        exec_ctx: &AvaExecCtx,
    ) -> Result<ExecOutcome, AvaEvmError> {
        let AvaEvmEnv { evm_env, header } = env;

        // 1. Apply atomic Import/Export effects to the overlay BEFORE the EVM tx
        //    loop (spec 10 §17.4). NoopPreHook on the pure-EVM reexecute path.
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

        // 3. A per-block `ConfigureEvm` whose factory carries THIS block's
        //    predicate results + P-Chain height (G4/G10): build the bare reth
        //    block executor over the parent `State` overlay and drive it —
        //    pre-execution changes -> ordered tx loop -> finish. The factory
        //    installs the height-gated Avalanche precompiles and the
        //    [`AvaHandler`] fee rule into the EVM it creates.
        let factory = AvaEvmFactory::new(
            self.chain_spec.clone(),
            self.precompiles.clone(),
            exec_ctx.clone(),
        );
        let inner = EthEvmConfig::new_with_evm_factory(self.exec_spec.clone(), factory);
        let evm = inner.evm_with_env(&mut *parent, evm_env);
        let mut executor = inner.create_executor(evm, ctx);

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

impl ExternalConsensusExecutor for AvaEvmConfig {
    type State = AvaState;

    fn execute_batch(
        &self,
        env: AvaEvmEnv,
        parent: &mut Self::State,
        pre_hook: &dyn PreExecutionHook,
        txs: &[RecoveredTx],
    ) -> Result<ExecOutcome, AvaEvmError> {
        // No predicate results threaded (the no-warp path): every
        // `getVerifiedWarpMessage` read returns invalid, coreth parity for a
        // tx with no predicates. Callers with warp predicates use
        // [`AvaEvmConfig::execute_batch_with_ctx`].
        self.execute_batch_with_ctx(env, parent, pre_hook, txs, &AvaExecCtx::default())
    }
}

// ---------------------------------------------------------------------------
// M6.31 — the live EvmFactory / AvaEvm / AvaHandler (G4/G10, spec 10 §6.5/§8/
// §17.1/§17.5; spec 21 §7)
// ---------------------------------------------------------------------------

/// The inner revm `Evm` an [`AvaEvm`] wraps: the alloy-evm Ethereum context
/// (`EthEvmContext<DB>`), Ethereum instruction set + frame, and the dynamic
/// [`PrecompilesMap`] the factory installs the Avalanche precompiles into.
type AvaRevmEvm<DB, I> = RevmEvm<
    EthEvmContext<DB>,
    I,
    EthInstructions<EthInterpreter, EthEvmContext<DB>>,
    PrecompilesMap,
    EthFrame,
>;

/// The Avalanche revm [`Handler`] (M6.31, spec 21 §7): identical to revm's
/// `MainnetHandler` except for the two Avalanche fee-rule deltas, ported from
/// coreth `core/state_transition.go`:
///
/// 1. **`reward_beneficiary`** credits the coinbase the FULL effective gas
///    price (base fee + tip) times gas used — Avalanche does **not** burn the
///    EIP-1559 base fee (coreth: `fee = gasUsed * msg.GasPrice;
///    AddBalance(coinbase, fee)`). revm's default discards the base-fee
///    portion post-London.
/// 2. **`refund`** is disabled entirely when ApricotPhase1 is active (coreth
///    `refundGas(apricotPhase1=true)` never refunds; pre-AP1 uses the
///    pre-London refund quotient, which the stock path already applies
///    because pre-AP1 maps to a pre-London spec id).
pub struct AvaHandler<EVM, ERROR, FRAME> {
    /// Whether gas refunds are disabled (ApricotPhase1+ — always on mainnet).
    disable_refund: bool,
    /// Generic-parameter carrier (no data).
    _phantom: PhantomData<fn() -> (EVM, ERROR, FRAME)>,
}

impl<EVM, ERROR, FRAME> AvaHandler<EVM, ERROR, FRAME> {
    /// A handler with the AP1 refund switch set from the active fork.
    #[must_use]
    pub fn new(disable_refund: bool) -> Self {
        Self {
            disable_refund,
            _phantom: PhantomData,
        }
    }
}

impl<EVM, ERROR, FRAME> Handler for AvaHandler<EVM, ERROR, FRAME>
where
    EVM: EvmTr<Context: ContextTr<Journal: JournalTr<State = EvmState>>, Frame = FRAME>,
    ERROR: EvmTrError<EVM>,
    FRAME: FrameTr<FrameResult = FrameResult, FrameInit = FrameInit>,
{
    type Evm = EVM;
    type Error = ERROR;
    type HaltReason = HaltReason;

    fn refund(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
        eip7702_refund: i64,
    ) {
        if self.disable_refund {
            // ApricotPhase1+: NO gas refunds at all (coreth `refundGas` skips
            // the refund counter entirely). Zero out anything the journal
            // accumulated (SSTORE clears, EIP-7702) so `gas.used()` — and the
            // caller reimbursement — match coreth's post-AP1 accounting.
            let gas = exec_result.gas_mut();
            let refunded = gas.refunded();
            if refunded != 0 {
                gas.record_refund(refunded.saturating_neg());
            }
        } else {
            // Pre-AP1: stock refund (the active spec is pre-London, so the
            // stock path applies the quotient-2 rule coreth uses there).
            let spec = evm.ctx().cfg().spec().into();
            post_execution::refund(spec, exec_result.gas_mut(), eip7702_refund);
        }
    }

    fn reward_beneficiary(
        &self,
        evm: &mut Self::Evm,
        exec_result: &mut <<Self::Evm as EvmTr>::Frame as FrameTr>::FrameResult,
    ) -> Result<(), Self::Error> {
        // coreth `state_transition.go`: `fee := gasUsed * msg.GasPrice` (the
        // EFFECTIVE gas price, base fee included) credited to the coinbase —
        // never burned. `gas.used()` is spent minus refunded (refunds are
        // zeroed above when AP1 is active), matching coreth's `st.gasUsed()`
        // after `refundGas`. Reservoir gas (EIP-8037) is unused-and-reimbursed,
        // so it is excluded exactly like revm's stock path.
        let gas = exec_result.gas();
        let effective_used = gas.used().saturating_sub(gas.reservoir());
        let ctx = evm.ctx();
        let (block, tx, _, journal, _, _) = ctx.all_mut();
        let basefee = u128::from(block.basefee());
        let effective_gas_price = tx.effective_gas_price(basefee);
        let fee = effective_gas_price.saturating_mul(u128::from(effective_used));
        journal
            .load_account_mut(block.beneficiary())?
            .incr_balance(U256::from(fee));
        Ok(())
    }
}

// The inspector pass-through: `AvaHandler` runs under `inspect_run` exactly like
// `MainnetHandler` (the fee-rule overrides live in the `Handler` methods, which
// the inspector run path shares).
impl<EVM, ERROR> InspectorHandler for AvaHandler<EVM, ERROR, EthFrame>
where
    EVM: InspectorEvmTr<
            Context: ContextTr<Journal: JournalTr<State = EvmState>>,
            Frame = EthFrame,
            Inspector: Inspector<<EVM as EvmTr>::Context, EthInterpreter>,
        >,
    ERROR: EvmTrError<EVM>,
{
    type IT = EthInterpreter;
}

/// The Avalanche EVM (M6.31, G4/G10): mirrors alloy-evm's `EthEvm` (same revm
/// `Evm` core over the Ethereum context) with two deltas — the factory installs
/// the Avalanche stateful precompiles into its [`PrecompilesMap`], and
/// `transact_raw` runs the [`AvaHandler`] (full-fee-to-coinbase, AP1 refund
/// rule) instead of revm's `MainnetHandler`.
pub struct AvaEvm<DB: RevmDatabase, I> {
    /// The bare revm `Evm`.
    inner: AvaRevmEvm<DB, I>,
    /// Whether [`Inspector`] hooks run on `transact`.
    inspect: bool,
    /// ApricotPhase1+ → gas refunds disabled (see [`AvaHandler`]).
    disable_refund: bool,
    /// Number of `transact_raw` calls so far — the next tx's index in the block.
    executed_txs: u64,
    /// The currently-executing tx index, shared with the precompile closures
    /// (the warp precompile reads its per-tx predicate results through it).
    current_tx_index: Arc<AtomicU64>,
}

impl<DB, I> ava_evm_reth::Evm for AvaEvm<DB, I>
where
    DB: RevmDatabase,
    I: Inspector<EthEvmContext<DB>>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = ava_evm_reth::SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.inner.ctx.block
    }

    fn cfg_env(&self) -> &CfgEnv<Self::Spec> {
        &self.inner.ctx.cfg
    }

    fn chain_id(&self) -> u64 {
        self.inner.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // Publish this tx's index within the block to the precompile closures
        // (PredicateResults are keyed by tx index, spec 20 §7.2) BEFORE running.
        self.current_tx_index
            .store(self.executed_txs, Ordering::SeqCst);
        self.executed_txs = self.executed_txs.saturating_add(1);

        self.inner.ctx.set_tx(tx);
        let mut handler = AvaHandler::new(self.disable_refund);
        let result = if self.inspect {
            handler.inspect_run(&mut self.inner)
        } else {
            handler.run(&mut self.inner)
        }?;
        let state = self.inner.ctx.journal_mut().finalize();
        Ok(ResultAndState::new(result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: ava_evm_reth::Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // System calls (EIP-4788/2935) pay no fee and reward no beneficiary —
        // revm's stock system-call path is already coreth-faithful.
        self.inner.system_call_with_caller(caller, contract, data)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context {
            block: block_env,
            cfg: cfg_env,
            journaled_state,
            ..
        } = self.inner.ctx;
        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.ctx.journaled_state.database,
            &self.inner.inspector,
            &self.inner.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector,
            &mut self.inner.precompiles,
        )
    }
}

/// The Avalanche [`EvmFactory`] (M6.31, G4/G10, spec 10 §8/§17.5): produces an
/// [`AvaEvm`] with (1) the fork+upgrade-activated Avalanche stateful
/// precompiles installed into the [`PrecompilesMap`] as `DynPrecompile`s
/// (gated on `block.timestamp >= module.activation`, §8.3) and (2) the
/// [`AvaHandler`] fee rule. The per-block [`AvaExecCtx`] carries the verified
/// warp predicate results + P-Chain height the closures thread into each
/// [`PrecompileCtx`].
#[derive(Clone, Debug)]
pub struct AvaEvmFactory {
    /// The fork schedule (Durango/AP1 gating for the precompile bodies + fee
    /// rule).
    chain_spec: Arc<AvaChainSpec>,
    /// The stateful-precompile registry (address → module + activation).
    registry: Arc<PrecompileRegistry>,
    /// This block's predicate results + P-Chain height.
    exec_ctx: AvaExecCtx,
}

impl AvaEvmFactory {
    /// Builds the factory for one block (or the empty-ctx `eth_call` path).
    #[must_use]
    pub fn new(
        chain_spec: Arc<AvaChainSpec>,
        registry: Arc<PrecompileRegistry>,
        exec_ctx: AvaExecCtx,
    ) -> Self {
        Self {
            chain_spec,
            registry,
            exec_ctx,
        }
    }

    /// Builds the [`PrecompilesMap`] for a block: revm's standard Ethereum set
    /// for the spec, overlaid with every registered Avalanche module whose
    /// activation timestamp has passed (spec 10 §8.3). Each module is wrapped
    /// in a `DynPrecompile` closure that adapts the alloy-evm call surface
    /// (`PrecompileInput` + `EvmInternals`) onto the crate's
    /// [`crate::precompile::registry::StatefulPrecompile`] trait.
    fn build_precompiles(
        &self,
        spec: ava_evm_reth::SpecId,
        block: AvaBlockCtx,
        current_tx_index: &Arc<AtomicU64>,
    ) -> PrecompilesMap {
        let mut map =
            PrecompilesMap::from_static(Precompiles::new(PrecompileSpecId::from_spec_id(spec)));
        for module in self
            .registry
            .modules()
            .filter(|m| block.timestamp >= m.activation)
        {
            let precompile = module.precompile.clone();
            let predicates = self.exec_ctx.predicates.clone();
            let tx_index = current_tx_index.clone();
            map.apply_precompile(&module.address, move |_| {
                Some(DynPrecompile::new_stateful(
                    PrecompileId::custom("avalanche-stateful"),
                    move |input: PrecompileInput<'_>| -> PrecompileResult {
                        let PrecompileInput {
                            data,
                            gas,
                            reservoir,
                            caller,
                            value,
                            is_static,
                            mut internals,
                            ..
                        } = input;
                        let pctx = PrecompileCtx {
                            caller,
                            value,
                            read_only: is_static,
                            predicates: predicates.clone(),
                            block: AvaBlockCtx {
                                current_tx_index: tx_index.load(Ordering::SeqCst),
                                ..block
                            },
                        };
                        let mut ops = InternalsStateOps(&mut internals);
                        let result = precompile.run(data, gas, &pctx, &mut ops)?;
                        Ok(interpreter_result_to_precompile_output(result, reservoir))
                    },
                ))
            });
        }
        map
    }

    /// Builds the [`AvaEvm`] for `input` (shared by both factory entry points).
    fn build_evm<DB: RevmDatabase, I: Inspector<EthEvmContext<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
        inspect: bool,
    ) -> AvaEvm<DB, I> {
        let timestamp = u64::try_from(input.block_env.timestamp).unwrap_or(u64::MAX);
        let block_number = u64::try_from(input.block_env.number).unwrap_or(u64::MAX);
        let phase = self.chain_spec.fork_at(timestamp);
        let block = AvaBlockCtx {
            pchain_height: self.exec_ctx.pchain_height,
            timestamp,
            current_tx_index: 0,
            block_number,
            is_durango: phase >= AvaPhase::Durango,
        };
        let current_tx_index = Arc::new(AtomicU64::new(0));
        let precompiles = self.build_precompiles(input.cfg_env.spec, block, &current_tx_index);

        let inner = Context::mainnet()
            .with_block(input.block_env)
            .with_cfg(input.cfg_env)
            .with_db(db)
            .build_mainnet_with_inspector(inspector)
            .with_precompiles(precompiles);

        AvaEvm {
            inner,
            inspect,
            disable_refund: phase >= AvaPhase::ApricotPhase1,
            executed_txs: 0,
            current_tx_index,
        }
    }
}

impl EvmFactory for AvaEvmFactory {
    type Evm<DB: RevmDatabase, I: Inspector<EthEvmContext<DB>>> = AvaEvm<DB, I>;
    type Context<DB: RevmDatabase> = EthEvmContext<DB>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = ava_evm_reth::SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: RevmDatabase>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        self.build_evm(db, input, NoOpInspector {}, false)
    }

    fn create_evm_with_inspector<DB: RevmDatabase, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        self.build_evm(db, input, inspector, true)
    }
}
