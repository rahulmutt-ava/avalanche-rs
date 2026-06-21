// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.28 — the `avalanchers` binary can assemble an in-process chain manager,
//! register a built-in no-op test-VM factory, create one Snowman chain through
//! the full `create_snowman_chain` pipeline, and report the chain's
//! last-accepted height. `--version` / `--help` must still answer (exit 0).

use std::process::Command;

use ava_genesis::Chain;
use ava_snow::EngineState;
use avalanchers::wiring::chains::{
    Sent, boot_in_process_pchain, boot_in_process_pchain_to_normalop, build_in_process_chain,
    register_test_vm_factory,
};

/// The binary builds a chain manager, registers the no-op test-VM factory,
/// creates an in-process Snowman chain, and reports its last-accepted height.
#[tokio::test]
async fn binary_constructs_chain_manager() {
    // The manager registers the built-in no-op test-VM factory under its fixed
    // VM id (probing the VM's `Version`/`Shutdown` once).
    let manager = register_test_vm_factory()
        .await
        .expect("register the built-in test-VM factory");
    assert_eq!(
        manager.list_factories().len(),
        1,
        "exactly one factory registered"
    );

    // The full create_snowman_chain pipeline assembles and the wrapped VM answers
    // its last-accepted height (genesis is height 0).
    let height = build_in_process_chain()
        .await
        .expect("assemble an in-process Snowman chain");
    assert_eq!(height, 0, "genesis is the last accepted block at height 0");
}

/// M4.30c — the binary materializes and boots the **real `PlatformVm`** seeded
/// from real P-Chain genesis in-process, driving the handler→engine-adapter
/// path until it enters `Bootstrapping` and broadcasts `GetAcceptedFrontier`
/// to its beacon set. The real ava-network-backed `Sender` is the documented
/// live arm; here a recording sender observes the frontier broadcast.
#[tokio::test]
async fn boots_real_pchain_to_bootstrapping() {
    // Mainnet (network_id 1) embedded P-Chain genesis (the M8-complete source).
    let network_id = 1u32;

    let handle = boot_in_process_pchain(network_id)
        .await
        .expect("boot the real P-Chain in-process");

    // The VM initialized from real genesis: the chain's genesis block id is the
    // expected `sha256(p_chain_genesis_bytes)` (specs 23 §4).
    let expected_genesis =
        ava_genesis::genesis_block_id(network_id, Chain::P).expect("P-Chain genesis block id");
    assert_eq!(
        handle.genesis_id, expected_genesis,
        "VM initialized at the real P-Chain genesis"
    );

    // Poll the shared ConsensusContext until the handler flips the engine into
    // `Bootstrapping` (virtual time; bounded yield loop, no wall-clock sleep).
    let mut entered = false;
    for _ in 0..10_000 {
        if matches!(**handle.ctx.state.load(), EngineState::Bootstrapping) {
            entered = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(entered, "the engine entered Bootstrapping");

    // The bootstrapper broadcast `GetAcceptedFrontier` to its beacon set.
    let log = handle.sender.drain();
    let frontier = log.iter().find_map(|s| match s {
        Sent::GetAcceptedFrontier { nodes, .. } => Some(nodes.clone()),
        _ => None,
    });
    let frontier = frontier.expect("bootstrapper broadcast GetAcceptedFrontier");
    assert_eq!(
        frontier, handle.beacons,
        "GetAcceptedFrontier addressed the beacon node set"
    );

    // No leaked task: cancel and join cleanly.
    handle.token.cancel();
    handle.join.await.expect("handler task joined cleanly");
}

/// M9.15 step (a) — the real `PlatformVm` reaches **`NormalOp`** through the
/// full `create_snowman_chain` pipeline + handler when booted as a SOLO node
/// (empty beacon set). This is the load-bearing proof that a single Rust node
/// can finish bootstrap WITHOUT the live ava-network `Sender`: the bootstrapper
/// short-circuits `Bootstrapping → NormalOp` when there is nothing to fetch
/// (`ava_engine::snowman::bootstrap` empty-beacon path), exactly as a Go
/// `--network-id=local` node with no default beacons does. The production
/// node-assembly chain-creator (driving the live binary's queued chains) will
/// replicate this template (see plan/M9.15 LIVE-ARM SCOPING).
#[tokio::test]
async fn boots_real_pchain_to_normalop() {
    // Mainnet (network_id 1) embedded P-Chain genesis (the M8-complete source).
    let network_id = 1u32;

    let handle = boot_in_process_pchain_to_normalop(network_id)
        .await
        .expect("boot the real P-Chain in-process (solo, empty beacons)");

    // The VM initialized at the real P-Chain genesis.
    let expected_genesis =
        ava_genesis::genesis_block_id(network_id, Chain::P).expect("P-Chain genesis block id");
    assert_eq!(
        handle.genesis_id, expected_genesis,
        "VM initialized at the real P-Chain genesis"
    );

    // A solo node has no beacons to fetch from, so the bootstrapper finishes
    // immediately and hands off to NormalOp. Poll the shared ConsensusContext
    // until it reaches `NormalOp` (virtual time; bounded yield loop).
    let mut reached = false;
    for _ in 0..100_000 {
        if matches!(**handle.ctx.state.load(), EngineState::NormalOp) {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        reached,
        "the solo engine reached NormalOp (last state: {:?})",
        **handle.ctx.state.load()
    );

    // A solo boot addresses no beacons (the short-circuit path).
    assert!(
        handle.beacons.is_empty(),
        "a solo node boots with an empty beacon set"
    );

    // No leaked task: cancel and join cleanly.
    handle.token.cancel();
    handle.join.await.expect("handler task joined cleanly");
}

/// M9.15 step (a), production wiring — the **chain creator** drives the
/// platform chain that step-26 `init_chains` *queued* on the
/// [`AssemblyChainManager`] all the way to `NormalOp`, and the manager's
/// `is_bootstrapped(P)` (the value `info.isBootstrapped` serves) flips from
/// `false` to `true` once the engine reaches `NormalOp`. Scoped to the P-Chain
/// (solo, empty-beacon short-circuit; the real ava-network `Sender` and X/C/SAE
/// dispatch are the documented deferrals).
#[tokio::test]
async fn chain_creator_drives_queued_pchain_to_bootstrapped() {
    use std::sync::Arc;

    use ava_node::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID, init_chains};
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::run_queued_chains;

    let network_id = 1u32; // mainnet embedded P-Chain genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build P-Chain genesis bytes");

    // Assemble the chain manager exactly as `init_chain_manager` does (critical
    // set = {P}, beacons = empty primary-network manager for a solo node).
    let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
    let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));

    // Step 26: queue the platform chain (plus the genesis X- and C-Chains).
    // Nothing runs yet, nothing is bootstrapped — the documented pre-wiring
    // state (wave-18h).
    init_chains(&manager, &genesis_bytes).expect("queue the platform chain");
    assert!(
        !manager.is_bootstrapped(PLATFORM_CHAIN_ID),
        "no chain runs before the chain creator drives the queue"
    );
    assert_eq!(
        manager.running_chains(),
        0,
        "no chain is registered before the chain creator runs"
    );

    // The chain creator constructs + drives the queued chains through the full
    // create_snowman_chain pipeline and reflects each ConsensusContext into the
    // manager's is_bootstrapped. P, X and C all boot.
    let handles = run_queued_chains(&manager, network_id)
        .await
        .expect("the chain creator boots the queued P-, X- and C-Chains");
    assert_eq!(handles.len(), 3, "the P-, X- and C-Chains boot");
    assert_eq!(
        manager.running_chains(),
        3,
        "the booted P-, X- and C-Chains are registered as running chains"
    );

    // Poll the manager until is_bootstrapped(P) flips (virtual time; bounded
    // yield loop). A solo node short-circuits Bootstrapping → NormalOp.
    let mut flipped = false;
    for _ in 0..200_000 {
        if manager.is_bootstrapped(PLATFORM_CHAIN_ID) {
            flipped = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        flipped,
        "info.isBootstrapped(P) flips true once the solo engine reaches NormalOp"
    );

    // Clean shutdown: the chain's handler runs under the manager-registered
    // token, so the manager's shutdown cancels it; then the handler joins.
    manager.shutdown(std::time::Duration::from_secs(5)).await;
    for handle in handles {
        handle.join.await.expect("handler task joined cleanly");
    }
}

/// M9.15 X/C dispatch — the chain creator dispatches by `vm_id`: it boots both
/// the queued **P-Chain** (`platform_vm_id`) and the queued **X-Chain**
/// (`avm_id`) AND the **C-Chain** (`evm_id`) to `NormalOp`, flipping
/// `is_bootstrapped(P)`, `is_bootstrapped(X)` and `is_bootstrapped(C)` true. The
/// C-Chain boots through the `EvmVm::from_genesis` construction seam (M6.8
/// genesis wiring), so all three critical chains register with the manager
/// (`running_chains() == 3`) and drain cleanly on shutdown.
#[tokio::test]
async fn chain_creator_dispatches_xchain_to_bootstrapped() {
    use std::sync::Arc;

    use ava_genesis::Chain;
    use ava_node::init::chain_manager::{
        AssemblyChainManager, PLATFORM_CHAIN_ID, avm_id, evm_id, init_chains, platform_vm_id,
    };
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::run_queued_chains;

    let network_id = 1u32; // mainnet embedded P-Chain genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build P-Chain genesis bytes");

    // The X/C blockchain ids are the genesis `CreateChainTx` ids (specs 23 §4).
    let x_chain_id = ava_genesis::genesis_block_id(network_id, Chain::X).expect("X blockchain id");
    let c_chain_id = ava_genesis::genesis_block_id(network_id, Chain::C).expect("C blockchain id");

    // Critical set = {P, X, C} (init_chain_manager's set); beacons empty (solo).
    let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    let critical = [PLATFORM_CHAIN_ID, x_chain_id, c_chain_id]
        .into_iter()
        .collect();
    let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));

    // Step 26 queues the platform chain AND the two standard chains the genesis
    // spawns — X (avm, real production genesis) and C (evm) — directly off the
    // genesis `CreateChainTx`s, so the per-`vm_id` dispatch is exercised
    // end-to-end from real genesis (no synthetic seed).
    init_chains(&manager, &genesis_bytes).expect("queue the platform, X- and C-Chains");
    assert_eq!(
        manager.queued_chains().len(),
        3,
        "init_chains queues P, X and C"
    );

    // Sanity: distinct, well-known VM ids.
    assert_ne!(platform_vm_id(), avm_id(), "P and X VM ids differ");
    assert_ne!(avm_id(), evm_id(), "X and C VM ids differ");

    let handles = run_queued_chains(&manager, network_id)
        .await
        .expect("the chain creator boots the queued P-, X- and C-Chains");
    assert_eq!(handles.len(), 3, "P, X and C all boot");
    assert_eq!(
        manager.running_chains(),
        3,
        "the P-, X- and C-Chains are all registered as running"
    );

    // Poll until P, X and C all flip bootstrapped (solo ⇒ Bootstrapping → NormalOp).
    let mut p_flipped = false;
    let mut x_flipped = false;
    let mut c_flipped = false;
    for _ in 0..400_000 {
        p_flipped |= manager.is_bootstrapped(PLATFORM_CHAIN_ID);
        x_flipped |= manager.is_bootstrapped(x_chain_id);
        c_flipped |= manager.is_bootstrapped(c_chain_id);
        if p_flipped && x_flipped && c_flipped {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(p_flipped, "info.isBootstrapped(P) flips true at NormalOp");
    assert!(x_flipped, "info.isBootstrapped(X) flips true at NormalOp");
    assert!(c_flipped, "info.isBootstrapped(C) flips true at NormalOp");

    manager.shutdown(std::time::Duration::from_secs(5)).await;
    for handle in handles {
        handle.join.await.expect("handler task joined cleanly");
    }
}

/// M9.15 live-dispatch wiring — `drive_startup_chains` is the node-startup
/// entrypoint the `avalanchers` binary's `dispatch` path calls. It drives the
/// queued P-Chain to `NormalOp` for a **beaconless** (solo) node — so a live
/// `avalanchers --network-id=local` process reflects the running engine through
/// `info.isBootstrapped` — and **skips entirely** for a node with configured
/// bootstrap beacons (whose real bootstrap is the deferred live-`Sender` path),
/// leaving `info.isBootstrapped` honestly `false` rather than falsely
/// short-circuiting.
#[tokio::test]
async fn drive_startup_chains_gates_on_beacons() {
    use std::sync::Arc;

    use ava_node::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID, init_chains};
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::drive_startup_chains;

    let network_id = 1u32; // mainnet embedded P-Chain genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build P-Chain genesis bytes");

    // A node WITH configured beacons: the creator defers (the live-Sender
    // bootstrap path is the documented live arm) — nothing runs, nothing
    // bootstraps, so `info.isBootstrapped` stays honestly false.
    {
        let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
        let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
        let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
        init_chains(&manager, &genesis_bytes).expect("queue the platform chain");

        let handles = drive_startup_chains(&manager, network_id, /* beaconless = */ false)
            .await
            .expect("the gating skip is infallible");
        assert!(
            handles.is_empty(),
            "a beaconed node defers chain creation to the live-Sender bootstrap path"
        );
        assert_eq!(
            manager.running_chains(),
            0,
            "no chain runs for a beaconed node"
        );
        assert!(
            !manager.is_bootstrapped(PLATFORM_CHAIN_ID),
            "info.isBootstrapped(P) stays false (honest) for a beaconed node"
        );
    }

    // A beaconless (solo) node: the creator drives the queued chains to
    // NormalOp and `info.isBootstrapped(P)` flips true. P, X and C all boot.
    {
        let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
        let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
        let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
        init_chains(&manager, &genesis_bytes).expect("queue the platform chain");

        let handles = drive_startup_chains(&manager, network_id, /* beaconless = */ true)
            .await
            .expect("the creator drives the solo P-, X- and C-Chains");
        assert_eq!(handles.len(), 3, "the solo P-, X- and C-Chains boot");
        assert_eq!(
            manager.running_chains(),
            3,
            "the booted P-, X- and C-Chains are registered as running chains"
        );

        let mut flipped = false;
        for _ in 0..200_000 {
            if manager.is_bootstrapped(PLATFORM_CHAIN_ID) {
                flipped = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            flipped,
            "info.isBootstrapped(P) flips true once the solo engine reaches NormalOp"
        );

        manager.shutdown(std::time::Duration::from_secs(5)).await;
        for handle in handles {
            handle.join.await.expect("handler task joined cleanly");
        }
    }
}

/// M9.15 restart persistence — a node restart re-opens the **same** persistent
/// base db and resumes. Boot the queued P-, X- and C-Chains over a shared base
/// db, drive to `NormalOp`, shut the node down cleanly, then re-boot a **fresh**
/// chain manager over the **same** base db — the real restart shape (a new
/// process, the same on-disk backend). The second boot must reach `NormalOp`
/// again *over the now-non-empty db* (the re-open path — the existing
/// [`run_queued_chains_persists_into_supplied_base_db`] only covers a boot over
/// an empty db), and every key the first boot persisted must still be present
/// with the same value: the persisted tip resumes across the restart.
///
/// **As-built scope (the resumed tip is genesis, height 0).** The booted chains
/// re-derive their last-accepted from genesis on each `initialize` rather than
/// loading an advanced tip from disk: `ava_platformvm::state::State::new` starts
/// with empty in-memory caches (no load-from-disk), `PlatformVm::initialize`
/// re-seeds genesis unconditionally (no `IsInitialized` guard), and
/// `create_snowman_chain` roots consensus at the inner VM's freshly-re-seeded
/// last-accepted at height 0. So this test pins the persistence round-trip that
/// **is** guaranteed today — writes land in the shared backend, survive a clean
/// shutdown, and the re-open path is consistent and idempotent. Advancing the
/// tip past genesis and resuming an *advanced* tip is the documented deferral
/// (needs the inner-VM persisted-state load path + in-process block issuance);
/// see plan/M9.15.
#[tokio::test]
async fn node_restart_resumes_persisted_tip_over_shared_base_db() {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use ava_database::{DynDatabase, MemDb};
    use ava_node::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID, init_chains};
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::run_queued_chains_with_db;

    // Snapshot every (key, value) currently in the base db.
    fn snapshot(db: &Arc<dyn DynDatabase>) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let mut it = db.new_iterator_with_start_and_prefix(&[], &[]);
        let mut out = BTreeMap::new();
        while it.next() {
            match (it.key(), it.value()) {
                (Some(k), Some(v)) => {
                    out.insert(k.to_vec(), v.to_vec());
                }
                _ => break,
            }
        }
        out
    }

    // Build a fresh chain manager that has queued the P-, X- and C-Chains off
    // the real genesis — exactly what a fresh `avalanchers` process does at
    // startup (step 26 `init_chains`).
    fn fresh_manager(genesis_bytes: &[u8]) -> Arc<AssemblyChainManager> {
        let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
        let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
        let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
        init_chains(&manager, genesis_bytes).expect("queue the P-, X- and C-Chains");
        manager
    }

    let network_id = 1u32; // mainnet embedded genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build genesis bytes");

    // Poll the manager until is_bootstrapped(P) flips (virtual time; bounded
    // yield loop). A solo node short-circuits Bootstrapping → NormalOp.
    async fn await_bootstrapped(manager: &Arc<AssemblyChainManager>) {
        for _ in 0..400_000 {
            if manager.is_bootstrapped(PLATFORM_CHAIN_ID) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("info.isBootstrapped(P) flips true once the solo engine reaches NormalOp");
    }

    // The node's single persistent base db — the Arc survives the restart, just
    // as a real on-disk backend (rocksdb/leveldb) survives a process restart.
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());

    // ---- Boot 1: a fresh node over the empty base db ----
    let manager1 = fresh_manager(&genesis_bytes);
    let handles1 = run_queued_chains_with_db(&manager1, network_id, Arc::clone(&base))
        .await
        .expect("boot 1: the queued P-, X- and C-Chains over the empty base db");
    assert_eq!(handles1.len(), 3, "boot 1: P, X and C all boot");
    await_bootstrapped(&manager1).await;

    // Shut the node down cleanly (the manager-registered chains drain), keeping
    // the base db alive — this is the on-disk state a restart re-opens.
    manager1.shutdown(std::time::Duration::from_secs(5)).await;
    for handle in handles1 {
        handle
            .join
            .await
            .expect("boot 1: handler task joined cleanly");
    }

    // The first boot persisted consensus / VM metadata into the shared base db,
    // and a clean shutdown did NOT clear it.
    let persisted = snapshot(&base);
    assert!(
        !persisted.is_empty(),
        "boot 1 persisted state into the base db, surviving a clean shutdown"
    );

    // ---- Boot 2: restart — a FRESH chain manager over the SAME base db ----
    let manager2 = fresh_manager(&genesis_bytes);
    let handles2 = run_queued_chains_with_db(&manager2, network_id, Arc::clone(&base))
        .await
        .expect("boot 2: re-open the persisted (non-empty) base db");
    assert_eq!(handles2.len(), 3, "boot 2: P, X and C all re-boot");

    // The re-open path reaches NormalOp again *over the non-empty db* — booting
    // over a pre-seeded base db does not choke (no duplicate-init / must-be-empty
    // failure).
    await_bootstrapped(&manager2).await;

    // The persisted tip resumed: every key the first boot wrote is still present
    // with the same value (the genesis tip is re-derived consistently — nothing
    // was wiped, and the re-derivation is deterministic).
    let after = snapshot(&base);
    for (k, v) in &persisted {
        assert_eq!(
            after.get(k),
            Some(v),
            "the persisted tip key survived the restart unchanged"
        );
    }

    manager2.shutdown(std::time::Duration::from_secs(5)).await;
    for handle in handles2 {
        handle
            .join
            .await
            .expect("boot 2: handler task joined cleanly");
    }
}

/// M9.15 STEP (m) — **engine-driven block issuance**. With the self-loopback
/// installed, signalling [`VmEvent::PendingTxs`] on the VM→engine channel makes
/// the running Snowman engine `build_block`, issue it, and — because the loopback
/// closes the solo node's `k=1`/`β=1` poll (the engine's own `push_query` is
/// delivered back as an inbound query, answered with self-`Chits`) — **accept**
/// the block through the genuine consensus path. The chain tip advances from
/// genesis (height 0) to height 1 with **no direct `accept()` on the VM**: the
/// engine did it.
///
/// This is the property STEP (l) deferred: STEP (l) drove `build → verify →
/// accept` directly on a bare `PlatformVm` precisely because, without the
/// loopback, a self-built block is built + issued but never voted, so it stays
/// *processing* and the tip never advances. (Confirmed: flipping the loopback off
/// makes this test time out at height 0.) Here the block reaches acceptance
/// through the real handler → engine → poll machinery.
#[tokio::test]
async fn engine_accepts_self_built_block_via_loopback() {
    use std::sync::Arc;

    use ava_database::{DynDatabase, MemDb};
    use ava_node::init::chain_manager::PLATFORM_CHAIN_ID;
    use ava_types::id::Id;
    use ava_vm::testutil::TestVm;
    use ava_vm::vm::VmEvent;
    use avalanchers::wiring::chains::boot_chain_with_loopback;

    // A trivial VM whose `build_block` appends a child of the preferred tip; its
    // shared accepted state is observable after the VM is moved into the chain.
    let vm = TestVm::new();
    let observer = vm.observer();
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());

    let handle = boot_chain_with_loopback(
        1, // network_id
        PLATFORM_CHAIN_ID,
        ava_types::constants::PRIMARY_NETWORK_ID,
        "P",
        Id::EMPTY, // avax_asset_id — unused by TestVm
        Id::EMPTY, // genesis_id — unused by TestVm (it seeds its own genesis)
        vm,
        Vec::new(), // TestVm ignores genesis bytes
        Arc::clone(&base),
    )
    .await
    .expect("boot a TestVm chain with the self-loopback installed");

    // Solo node (empty beacons) short-circuits Bootstrapping → NormalOp.
    let mut reached = false;
    for _ in 0..200_000 {
        if matches!(**handle.ctx.state.load(), EngineState::NormalOp) {
            reached = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        reached,
        "the solo engine reached NormalOp (last state: {:?})",
        **handle.ctx.state.load()
    );

    // The tip is genesis (height 0) before any issuance.
    assert_eq!(
        observer.last_accepted_height(),
        0,
        "the chain tip is genesis before issuance"
    );
    let genesis_id = observer.last_accepted_id();

    // Signal pending txs: the engine builds + issues + (via the loopback's
    // self-chits) accepts a height-1 block.
    handle
        .vm_tx
        .send(VmEvent::PendingTxs)
        .await
        .expect("signal PendingTxs to the running engine");

    // Poll until the engine-accepted block advances the tip to height 1.
    let mut advanced = false;
    for _ in 0..2_000_000 {
        if observer.last_accepted_height() == 1 {
            advanced = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        advanced,
        "the engine built, issued, and ACCEPTED a height-1 block through the \
         loopback poll path (tip height: {})",
        observer.last_accepted_height()
    );
    assert_ne!(
        observer.last_accepted_id(),
        genesis_id,
        "the new tip is a freshly-built child block, not genesis"
    );

    // No leaked task: cancel and join cleanly.
    handle.token.cancel();
    handle.join.await.expect("handler task joined cleanly");
}

/// `--version` and `--help` keep working unchanged (the M0 invariant).
#[test]
fn version_and_help_still_work() {
    let exe = env!("CARGO_BIN_EXE_avalanchers");

    let v = Command::new(exe).arg("--version").output().unwrap();
    assert!(v.status.success(), "--version exits 0");
    let stdout = String::from_utf8_lossy(&v.stdout);
    let version = &*ava_version::CURRENT;
    let expected = format!(
        "avalanchers/{}.{}.{}",
        version.major, version.minor, version.patch
    );
    assert!(
        stdout.contains(&expected),
        "--version prints {expected:?}, got {stdout:?}"
    );

    let h = Command::new(exe).arg("--help").output().unwrap();
    assert!(h.status.success(), "--help exits 0");
}

/// Real-DB threading — the chain creator boots every queued chain over **one
/// shared persistent base db** (Go's model: a single base DB, a `prefixdb` per
/// chain) rather than each chain using its own ephemeral in-memory db. The
/// caller-supplied `Arc<dyn DynDatabase>` is empty before boot and non-empty
/// after, proving the booted P-, X- and C-Chains persisted their consensus / VM
/// state into it (each namespaced under `build_db_stack`'s per-chain prefix).
/// This is the prerequisite for live-node restart persistence — a solo
/// `avalanchers` node now threads its real `node.db` through this path.
#[tokio::test]
async fn run_queued_chains_persists_into_supplied_base_db() {
    use std::sync::Arc;

    use ava_database::{DynDatabase, MemDb};
    use ava_node::init::chain_manager::{AssemblyChainManager, PLATFORM_CHAIN_ID, init_chains};
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::run_queued_chains_with_db;

    fn db_has_keys(db: &Arc<dyn DynDatabase>) -> bool {
        db.new_iterator_with_start_and_prefix(&[], &[]).next()
    }

    let network_id = 1u32; // mainnet embedded genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build genesis bytes");

    let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
    let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
    init_chains(&manager, &genesis_bytes).expect("queue the P-, X- and C-Chains");

    // One shared base db for the whole node, threaded into the chain creator.
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    assert!(
        !db_has_keys(&base),
        "run_queued_chains_with_db(): the base db is empty before any chain boots"
    );

    let handles = run_queued_chains_with_db(&manager, network_id, Arc::clone(&base))
        .await
        .expect("boot the queued P-, X- and C-Chains over the supplied base db");
    assert_eq!(
        handles.len(),
        3,
        "P, X and C all boot over the shared base db"
    );

    assert!(
        db_has_keys(&base),
        "run_queued_chains_with_db(): the booted chains persisted state into the supplied base db"
    );

    manager.shutdown(std::time::Duration::from_secs(5)).await;
    for handle in handles {
        handle.join.await.expect("handler task joined cleanly");
    }
}
