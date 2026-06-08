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
//!   reports `final_paris_total_difficulty == 0`, but does not list Paris in its
//!   timestamp schedule, so reth's `base_block_reward` (keyed on
//!   `is_paris_active_at_block`) would otherwise mint a spurious 5-ETH reward.
//!   `AvaExecutorSpec` forces Paris + all pre-merge Ethereum forks active at
//!   genesis (block 0) so the executor applies **no** block reward, matching
//!   coreth (SPEC FINDING M6.6 — see report).
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
    Hardfork, Hardforks, Head, Header, NodeRecord, PreExecutionHook, RecoveredTx, State,
    StateDbError, StateProviderDatabase, U256,
};

use crate::chainspec::AvaChainSpec;
use crate::state::FirewoodStateView;

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
/// bound (`EthChainSpec + EthExecutorSpec + Hardforks`) and pins the Avalanche
/// executor fork semantics (always post-merge; no PoW block reward — see module
/// docs / M6.6 SPEC FINDING). All `EthChainSpec` reads delegate to the inner
/// spec; only the Ethereum fork-activation view is adjusted.
#[derive(Clone, Debug)]
pub struct AvaExecutorSpec(Arc<AvaChainSpec>);

impl AvaExecutorSpec {
    /// The Ethereum forks Avalanche treats as active from genesis (block 0):
    /// every pre-merge fork **plus Paris/MergeNetsplit** (Avalanche is never
    /// PoW). Shanghai and later follow the inner timestamp schedule.
    fn is_forced_genesis_fork(fork: EthereumHardfork) -> bool {
        matches!(
            fork,
            EthereumHardfork::Frontier
                | EthereumHardfork::Homestead
                | EthereumHardfork::Dao
                | EthereumHardfork::Tangerine
                | EthereumHardfork::SpuriousDragon
                | EthereumHardfork::Byzantium
                | EthereumHardfork::Constantinople
                | EthereumHardfork::Petersburg
                | EthereumHardfork::Istanbul
                | EthereumHardfork::MuirGlacier
                | EthereumHardfork::Berlin
                | EthereumHardfork::London
                | EthereumHardfork::ArrowGlacier
                | EthereumHardfork::GrayGlacier
                | EthereumHardfork::Paris
        )
    }
}

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
        if Self::is_forced_genesis_fork(fork) {
            // Active from genesis: Avalanche is post-merge from block 0, so reth's
            // block-reward / merge checks (keyed on `_at_block`) resolve correctly.
            ForkCondition::Block(0)
        } else {
            // Shanghai+ follow the inner Avalanche timestamp schedule.
            self.0.ethereum_fork_activation(fork)
        }
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
}

impl AvaEvmConfig {
    /// Builds the config from an [`AvaChainSpec`].
    #[must_use]
    pub fn new(chain_spec: AvaChainSpec) -> Self {
        let spec = AvaExecutorSpec(Arc::new(chain_spec));
        Self {
            inner: EthEvmConfig::new(Arc::new(spec)),
        }
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
