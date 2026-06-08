// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`AvaPrecompiles`] — the revm [`PrecompileProvider`] that overlays the
//! Avalanche stateful precompiles on revm's standard Ethereum set — plus the
//! [`PrecompileRegistry`], the [`StatefulPrecompile`] trait, and the
//! [`AvaCtxExt`] revm context extension (G4/G10, spec 10 §8/§17.5/§17.11).
//!
//! ## Design (spec 10 §17.5, G4)
//!
//! revm dispatches precompiles through the [`PrecompileProvider`] trait
//! (address → precompile, on the revm handler). The Avalanche precompiles
//! (warp, allowlist, feemanager, nativeminter, rewardmanager) are **stateful**
//! (they read/write the live journaled EVM state) and **fork+upgrade gated**
//! (enabled by genesis/upgrade config at a given block timestamp, §8.3). We
//! implement a custom provider that, for a call:
//!
//! 1. checks whether the target address is in the **`warm`** set — the
//!    fork+upgrade-activated set [`AvaPrecompiles::for_height`] computes from the
//!    timestamp-keyed upgrade schedule — and is **registered** in the
//!    [`PrecompileRegistry`]; if so, dispatches to that [`StatefulPrecompile`];
//! 2. otherwise **falls through** to revm's standard [`EthPrecompiles`] for the
//!    active [`SpecId`].
//!
//! The warp precompile needs *pre-verified* off-EVM data (a warp message's BLS
//! aggregate verified against a P-Chain validator set at a height). revm's
//! provider `run` only receives the execution context, so we thread that data
//! through a revm **context extension** — [`AvaCtxExt`] — carried on the revm
//! context's `Chain` slot ([`ava_evm_reth::ContextTr::Chain`], the G10 churn
//! point). **M6.21 only plumbs the extension**; the pre-execution predicate pass
//! that populates [`AvaCtxExt::predicates`] and the per-precompile bodies are
//! M6.22.
//!
//! ## revm API reality (SPEC FINDING vs §17.5)
//!
//! The pinned revm (`revm-handler` 18.x) [`PrecompileProvider`] differs from the
//! §17.5 sketch: `set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec)` (generic
//! over the context's spec, not a bare `SpecId`); `warm_addresses` returns a
//! boxed `Iterator<Item = Address>` (not `&HashSet`); the typed extension rides
//! on `ContextTr::Chain` (there is no `ctx.ext()` accessor — §17.11/G10).
//! [`ava_evm_reth::PrecompileError`] has only `Fatal`/`FatalAny` variants (no
//! `Other`). These are recorded for the orchestrator to fold into the spec.

use std::collections::BTreeMap;
use std::sync::Arc;

use ava_evm_reth::{
    Address, CallInputs, Cfg, ContextTr, EthPrecompiles, InterpreterResult, PrecompileError,
    PrecompileProvider, SpecId, U256,
};

/// Per-call context handed to a [`StatefulPrecompile::run`]: the immediate
/// caller, the call value, and (M6.22) the verified warp predicate results +
/// proposervm block context. M6.21 only reserves the fields; the predicate pass
/// that fills [`AvaCtxExt::predicates`] is M6.22.
#[derive(Clone, Debug)]
pub struct PrecompileCtx {
    /// The immediate caller of the precompile (`CallInputs::caller`).
    pub caller: Address,
    /// The call value (wei) attached to the precompile call.
    pub value: U256,
    /// The verified-predicate-results handle (warp BLS results, etc.). Empty
    /// until M6.22's pre-execution predicate pass fills it.
    pub predicates: Arc<PredicateResults>,
    /// The proposervm/P-Chain block context for this block. Defaulted here;
    /// M6.22 sources it from `Block::verify_with_context`.
    pub block: AvaBlockCtx,
}

/// The verified warp predicates for a single transaction: the raw predicate
/// chunk-bytes (one `Vec<u8>` per warp message, in access-list order) plus the
/// boolean verification result the predicate pass produced for each. Indexed by
/// the warp message's predicate index (spec 20 §7.2/§7.3).
#[derive(Clone, Debug, Default)]
pub struct WarpTxPredicates {
    /// The raw (chunked) predicate bytes per warp message — what
    /// `getVerifiedWarpMessage` re-parses and charges per-chunk gas over.
    pub predicates: Vec<Vec<u8>>,
    /// The verification result per predicate index: `true` iff the BLS-aggregate
    /// predicate verified against the source subnet's `WarpSet` (spec 20 §7.2).
    pub valid: Vec<bool>,
}

/// Verified off-EVM predicate results threaded into the precompile context
/// (G4, §17.5). Populated by M6.22's pre-execution predicate pass
/// ([`crate::precompile::warp::run_predicates`]); read by the warp precompile's
/// `getVerifiedWarpMessage`/`getVerifiedWarpBlockHash` selectors.
///
/// Keyed by the transaction's index within the block, then by the precompile
/// address (the warp address). `BTreeMap` keeps a deterministic ordering (no
/// `HashMap` in execution paths, 00 §6.1).
#[derive(Clone, Debug, Default)]
pub struct PredicateResults {
    /// Per-(tx, precompile-addr) verified warp predicates.
    pub by_tx: BTreeMap<u64, BTreeMap<Address, WarpTxPredicates>>,
}

impl PredicateResults {
    /// Records the warp predicates verified for transaction `tx_index` at the
    /// warp precompile address (spec 20 §7.2). `predicates` is the per-message
    /// chunk-bytes in access-list order; `valid[i]` is the verification result
    /// for `predicates[i]`.
    pub fn set_warp(&mut self, tx_index: u64, predicates: Vec<Vec<u8>>, valid: Vec<bool>) {
        self.by_tx.entry(tx_index).or_default().insert(
            crate::precompile::warp::WARP_PRECOMPILE_ADDRESS,
            WarpTxPredicates { predicates, valid },
        );
    }

    /// The verified warp predicates for `tx_index` at `addr`, if any.
    #[must_use]
    pub fn warp_for(&self, tx_index: u64, addr: &Address) -> Option<&WarpTxPredicates> {
        self.by_tx.get(&tx_index).and_then(|m| m.get(addr))
    }
}

/// The proposervm/P-Chain block context a stateful precompile may observe
/// (G4/G10, §17.5). **Reserved plumbing** for M6.22; defaulted here.
#[derive(Clone, Debug, Default)]
pub struct AvaBlockCtx {
    /// The P-Chain height the proposervm block was issued at (warp validator-set
    /// selection). 0 until M6.22 wires the proposervm context.
    pub pchain_height: u64,
    /// The block timestamp (unix seconds) — the value `for_height` gates on.
    pub timestamp: u64,
    /// The index of the transaction currently executing within the block.
    pub current_tx_index: u64,
}

/// The revm **context extension** (G10, §17.5/§17.11) threaded onto the revm
/// context's `Chain` slot when [`crate::evmconfig`] builds the EVM. Carries the
/// verified predicate results + block context the warp precompile reads.
///
/// **M6.21 reserves the fields**; the pre-execution predicate pass that fills
/// `predicates` (warp BLS verification against the P-Chain validator set) is
/// M6.22.
#[derive(Clone, Debug, Default)]
pub struct AvaCtxExt {
    /// `tx_index → precompile_addr → verified bytes` (filled by M6.22).
    pub predicates: Arc<PredicateResults>,
    /// proposervm/P-Chain block context (filled by M6.22).
    pub block_ctx: AvaBlockCtx,
}

/// A stateful Avalanche precompile (warp, allowlist, feemanager, …): runs
/// against the live EVM state with access to the per-call [`PrecompileCtx`]
/// (caller, value, verified predicate results). The concrete bodies are M6.22;
/// M6.21 defines the trait + the dispatch path.
pub trait StatefulPrecompile: Send + Sync {
    /// Execute the precompile over `input` with a `gas_limit`, returning the
    /// revm [`InterpreterResult`] (output bytes + gas accounting).
    ///
    /// # Errors
    ///
    /// Returns [`PrecompileError`] on an unrecoverable precompile failure.
    fn run(
        &self,
        input: &[u8],
        gas_limit: u64,
        ctx: &PrecompileCtx,
    ) -> Result<InterpreterResult, PrecompileError>;
}

/// One registered precompile module: its fixed address, the upgrade timestamp it
/// activates at (`block_timestamp >= activation`, inclusive, matching the
/// Avalanche `!t.Before(forkTime)` boundary), and the stateful implementation.
///
/// (subnet-evm models each precompile as a `Module` keyed by a config key; here
/// the registry is keyed by address and the activation is a timestamp — §8.1/§8.3.)
#[derive(Clone)]
pub struct PrecompileModule {
    /// The precompile's fixed contract address.
    pub address: Address,
    /// The upgrade timestamp (unix seconds) at/after which the module is active.
    pub activation: u64,
    /// The stateful implementation.
    pub precompile: Arc<dyn StatefulPrecompile>,
}

// `Arc<dyn StatefulPrecompile>` is not `Debug`; emit the observable, non-opaque
// fields only (the precompile body is rendered as an elided marker).
impl core::fmt::Debug for PrecompileModule {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PrecompileModule")
            .field("address", &self.address)
            .field("activation", &self.activation)
            .field("precompile", &"<dyn StatefulPrecompile>")
            .finish()
    }
}

/// The registry of Avalanche stateful precompiles (address → module). Built once
/// from genesis/upgrade config (§8.3); [`AvaPrecompiles::for_height`] reads it to
/// compute the activated `warm` set for a block timestamp.
#[derive(Clone, Default, Debug)]
pub struct PrecompileRegistry {
    /// All registered modules, keyed by address. `BTreeMap` for a deterministic
    /// iteration order (no `HashMap` in execution paths, 00 §6.1).
    modules: BTreeMap<Address, PrecompileModule>,
}

impl PrecompileRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            modules: BTreeMap::new(),
        }
    }

    /// Registers a module (last write wins for a given address).
    pub fn register(&mut self, module: PrecompileModule) {
        self.modules.insert(module.address, module);
    }

    /// The module registered at `address`, if any (regardless of activation).
    #[must_use]
    pub fn get(&self, address: &Address) -> Option<&PrecompileModule> {
        self.modules.get(address)
    }

    /// Iterates over every registered module.
    pub fn modules(&self) -> impl Iterator<Item = &PrecompileModule> {
        self.modules.values()
    }
}

/// The Avalanche revm [`PrecompileProvider`] (G4, §8/§17.5): overlays the
/// fork+upgrade-activated Avalanche stateful precompiles on revm's standard
/// Ethereum precompile set, falling through to the latter for any address not in
/// the activated `warm` set.
#[derive(Clone)]
pub struct AvaPrecompiles {
    /// revm's standard Ethereum precompiles for the active spec (fall-through).
    base: EthPrecompiles,
    /// The full registry (address → stateful module).
    modules: Arc<PrecompileRegistry>,
    /// The fork+upgrade-activated Avalanche precompile addresses at the height
    /// this provider was built for. Sorted + deduped; lookups are a binary search
    /// over a small set, so a `Vec` is cheaper than a hash set here.
    warm: Vec<Address>,
}

impl AvaPrecompiles {
    /// Builds the provider for a block timestamp `t`: the `warm` set is every
    /// registered module whose `activation <= t` (inclusive boundary). The base
    /// Ethereum set is initialised at the lowest spec; [`PrecompileProvider::set_spec`]
    /// (called by revm before execution) re-keys it to the block's actual spec.
    #[must_use]
    pub fn for_height(modules: Arc<PrecompileRegistry>, t: u64) -> Self {
        let mut warm: Vec<Address> = modules
            .modules()
            .filter(|m| t >= m.activation)
            .map(|m| m.address)
            .collect();
        warm.sort_unstable();
        warm.dedup();
        Self {
            // `set_spec` re-keys this to the block spec before any `run`; the
            // starting spec is irrelevant (LATEST is a safe non-panicking seed).
            base: EthPrecompiles::new(SpecId::default()),
            modules,
            warm,
        }
    }

    /// Whether `addr` is an **activated** Avalanche stateful precompile (in the
    /// `warm` set) — i.e. would dispatch to a registered [`StatefulPrecompile`]
    /// rather than fall through. Does not consider the base Ethereum set.
    #[must_use]
    pub fn contains_stateful(&self, addr: &Address) -> bool {
        self.warm.binary_search(addr).is_ok()
    }

    /// The registered [`StatefulPrecompile`] for `addr` **iff** it is activated
    /// (warm) and registered — exactly the dispatch decision [`PrecompileProvider::run`]
    /// makes before falling through to the base set.
    #[must_use]
    pub fn dispatch_stateful(&self, addr: &Address) -> Option<&Arc<dyn StatefulPrecompile>> {
        if self.contains_stateful(addr) {
            self.modules.get(addr).map(|m| &m.precompile)
        } else {
            None
        }
    }

    /// The activated Avalanche stateful precompile addresses (the `warm` set),
    /// excluding the standard Ethereum precompiles. Sorted + deduped.
    #[must_use]
    pub fn warm_addresses_vec(&self) -> Vec<Address> {
        self.warm.clone()
    }
}

impl<CTX: ContextTr> PrecompileProvider<CTX> for AvaPrecompiles {
    type Output = InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as Cfg>::Spec) -> bool {
        // Re-key the standard Ethereum set to the block's active spec. The
        // Avalanche `warm` set is height-gated at construction (`for_height`) and
        // does not depend on the revm spec, so it is unaffected. The fully
        // qualified path ties `EthPrecompiles`' own `set_spec` to *this* `CTX`.
        <EthPrecompiles as PrecompileProvider<CTX>>::set_spec(&mut self.base, spec)
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        // Dispatch to an activated Avalanche stateful precompile, else fall
        // through to revm's standard Ethereum set (spec 10 §8/§17.5).
        if let Some(precompile) = self.dispatch_stateful(&inputs.bytecode_address).cloned() {
            // M6.22 sources `predicates`/`block` from the `AvaCtxExt` carried on
            // `context` (the revm `Chain` extension, G10); M6.21 plumbs an empty
            // context so the dispatch path is exercised end-to-end.
            let pctx = PrecompileCtx {
                caller: inputs.caller,
                value: inputs.call_value(),
                predicates: Arc::new(PredicateResults::default()),
                block: AvaBlockCtx::default(),
            };
            let input = inputs.input.bytes(context);
            return precompile
                .run(input.as_ref(), inputs.gas_limit, &pctx)
                .map(Some)
                .map_err(|e| e.to_string());
        }
        self.base.run(context, inputs)
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = Address>> {
        // The standard Ethereum precompile addresses for the active spec PLUS the
        // activated Avalanche addresses (spec 10 §17.5).
        let mut addrs: Vec<Address> = self.base.warm_addresses().collect();
        addrs.extend(self.warm.iter().copied());
        Box::new(addrs.into_iter())
    }

    fn contains(&self, address: &Address) -> bool {
        self.contains_stateful(address) || self.base.contains(address)
    }
}
