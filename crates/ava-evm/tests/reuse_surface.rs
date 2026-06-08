// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M6.26 — the **reuse-surface contract** (spec 10 §16 / §17.10, 00 §11.1.5).
//!
//! "One EVM engine, two drivers." This is a **compile-level** test that the
//! reusable items SAE's future `ava-saevm-exec` (spec 11 §6) needs are reachable
//! through *stable public paths* — the crate-root `ava_evm::{…}` re-exports and
//! the `ava_evm_reth::{…}` facade — and that the EVM execution engine is
//! **drivable standalone**: open a parent view by root, build the revm overlay,
//! call [`AvaEvmConfig::execute_batch`] with a pre-hook + ordered txs, then
//! propose + commit the post-state through the Firewood handles — **without ever
//! naming `EvmVm`, `EvmBlock`, `BlockBuilderDriver`, or any `reth_*`/`revm` type
//! directly**.
//!
//! If this file stops compiling, the reuse contract has regressed: an item the
//! SAE driver depends on was made private or moved off its stable path, or the
//! standalone execute path now requires the synchronous C-Chain block lifecycle.

use std::str::FromStr;
use std::sync::Arc;

// --- The §17.10 facade surface (ava-evm-reth) ------------------------------
// SAE drives the EVM through these and *only* these reth-touching names.
use ava_evm_reth::{
    AccountInfo, Address, AvaEvmEnv, B256, BundleState, Chain, ExecOutcome,
    ExternalConsensusExecutor, HashedPostState, Header, KeccakKeyHasher, RecoveredTx, StateBuilder,
    StateProviderDatabase, U256,
};
// --- The §17.10 reusable surface (ava-evm), via STABLE CRATE-ROOT paths ----
// Every one of these MUST be reachable at `ava_evm::<Name>` (not buried under a
// submodule path) so the SAE crate can depend on a flat, stable surface.
use ava_evm::{
    AtomicStateHook, AvaChainSpec, AvaEvmConfig, AvaState, FirewoodStateProvider,
    FirewoodStateView, NoopPreHook, PrecompileRegistry, hashed_post_state_to_batchops,
};

// `ava-database` provides the side stores; not part of the reuse contract but
// needed to open a provider in-process for the standalone drive.
use ava_database::{DynDatabase, MemDb};

/// Mainnet C-Chain spec — enough to drive a trivial empty batch. The reuse
/// contract only requires the *engine* to be drivable; the fork schedule is
/// shared verbatim (§17.8).
fn chain_spec() -> AvaChainSpec {
    AvaChainSpec::c_chain(1, Chain::from_id(43_114))
}

/// **The reuse contract.** Prove the SAE-reusable items are public AND that the
/// EVM engine is drivable standalone — no `EvmVm`/`EvmBlock`/`BlockBuilderDriver`
/// anywhere in this function (compile-enforced: those names are never imported).
#[test]
fn sae_reusable_items_are_public() {
    // 1. Open a Firewood-ethhash state provider in-process (the state-of-record
    //    SAE's `Tracker` holds as `Arc<FirewoodStateProvider>`, spec 11 §7.1).
    let dir = tempfile::tempdir().expect("tempdir");
    let bytecode: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let block_hashes: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let provider: Arc<FirewoodStateProvider> =
        FirewoodStateProvider::open(dir.path(), bytecode, block_hashes).expect("open provider");

    // 2. Commit a one-account genesis through the deferred propose/commit handles
    //    (§17.2.2). `propose_from_bundle` → root, then `commit(root)` — exactly
    //    the path SAE settles each interval through.
    let alice = Address::from_str("0x000000000000000000000000000000000000a11c").expect("addr");
    let genesis: BundleState = BundleState::builder(0..=0)
        .state_present_account_info(
            alice,
            AccountInfo {
                balance: U256::from(1_000_000_000_u64),
                nonce: 0,
                ..Default::default()
            },
        )
        .build();
    let genesis_root: B256 = provider
        .propose_from_bundle(&genesis)
        .expect("propose genesis");
    provider.commit(genesis_root).expect("commit genesis");
    assert_eq!(
        provider.root(),
        genesis_root,
        "tip advanced to genesis root"
    );

    // 3. Open a parent view BY ROOT (the history window, §17.2 / §5.2) and wrap
    //    it into the revm overlay the executor runs over — `AvaState` is the
    //    public alias for that overlay type.
    let view: FirewoodStateView = provider
        .history_by_state_root(genesis_root)
        .expect("view by root");
    let mut state: AvaState = StateBuilder::new()
        .with_database(StateProviderDatabase::new(view))
        .with_bundle_update()
        .build();

    // 4. Drive the SAME executor SAE uses: `AvaEvmConfig` (`impl
    //    ExternalConsensusExecutor`) — `execute_batch` over an EMPTY ordered tx
    //    batch + a `NoopPreHook`, decoupled from any `ChainVm`/`EvmVm`.
    let config = AvaEvmConfig::new(chain_spec());
    let header = Header {
        number: 1,
        gas_limit: 8_000_000,
        timestamp: 1,
        beneficiary: alice,
        ..Default::default()
    };
    let env: AvaEvmEnv = config.evm_env_for_header(&header);
    let txs: &[RecoveredTx] = &[];
    let outcome: ExecOutcome =
        ExternalConsensusExecutor::execute_batch(&config, env, &mut state, &NoopPreHook, txs)
            .expect("execute empty batch standalone");

    // 5. Settle the post-state through the deferred-commit handles — propose,
    //    then commit — the way SAE folds an interval's delta into Firewood.
    let post_root: B256 = provider
        .propose_from_bundle(&outcome.bundle)
        .expect("propose post-state");
    provider.commit(post_root).expect("commit post-state");
    assert_eq!(
        provider.root(),
        post_root,
        "tip advanced to post-state root"
    );

    // 6. The conversion that guarantees identical state roots across both drivers
    //    (§17.2.1) is public and pure.
    let hashed = HashedPostState::from_bundle_state::<KeccakKeyHasher>(&outcome.bundle.state);
    let _ops = hashed_post_state_to_batchops(&hashed);

    // 7. The hook + precompile reuse points (§17.4/§17.5) are public and
    //    constructible standalone (SAE C-Chain reuses warp/atomic semantics).
    let _registry: PrecompileRegistry = PrecompileRegistry::default();
    // `AtomicStateHook` is a `PreExecutionHook` SAE's C-Chain driver supplies in
    // place of `NoopPreHook`; assert the type is nameable on the reuse path.
    let _hook_is_a_type = std::marker::PhantomData::<AtomicStateHook>;

    // 8. The shared fork schedule + spec-id selection (§17.8).
    let _spec_id = config.chain_spec().revm_spec_id(0);
}

/// Statically assert the executor trait object is usable behind the facade — the
/// SAE driver holds the engine as a `dyn ExternalConsensusExecutor`-shaped value,
/// never as a concrete `EvmVm`.
#[test]
fn executor_is_reusable_as_trait() {
    fn drive<E: ExternalConsensusExecutor<State = AvaState>>(_e: &E) {}
    let config = AvaEvmConfig::new(chain_spec());
    drive(&config);
}
