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

    // Step 26: queue the platform chain. Nothing runs yet, nothing is
    // bootstrapped — this is the documented pre-wiring state (wave-18h).
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

    // The chain creator constructs + drives the queued P-Chain through the full
    // create_snowman_chain pipeline and reflects its ConsensusContext into the
    // manager's is_bootstrapped.
    let handles = run_queued_chains(&manager, network_id)
        .await
        .expect("the chain creator boots the queued P-Chain");
    assert_eq!(handles.len(), 1, "exactly one P-Chain booted");
    assert_eq!(
        manager.running_chains(),
        1,
        "the booted P-Chain is registered as a running chain"
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
/// (`avm_id`) to `NormalOp`, flipping `is_bootstrapped(P)` and
/// `is_bootstrapped(X)` true, while a queued **C-Chain** (`evm_id`) is **skipped**
/// (its `EvmVm::initialize` genesis wiring is the M6.8 deferral) so
/// `is_bootstrapped(C)` stays honestly false. Both booted chains register with
/// the manager (`running_chains() == 2`) and drain cleanly on shutdown.
#[tokio::test]
async fn chain_creator_dispatches_xchain_to_bootstrapped() {
    use std::sync::Arc;

    use ava_chains::manager::ChainParameters;
    use ava_node::init::chain_manager::{
        AssemblyChainManager, PLATFORM_CHAIN_ID, avm_id, evm_id, init_chains, platform_vm_id,
    };
    use ava_types::constants::PRIMARY_NETWORK_ID;
    use ava_types::id::Id;
    use ava_validators::{DefaultManager, ValidatorManager};
    use avalanchers::wiring::chains::run_queued_chains;

    let network_id = 1u32; // mainnet embedded P-Chain genesis (M8-complete source).
    let (genesis_bytes, _avax_asset_id) =
        ava_genesis::genesis_bytes(network_id, None).expect("build P-Chain genesis bytes");

    // Critical set = {P, X, C} (init_chain_manager's set); beacons empty (solo).
    let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
    let x_chain_id = Id::from([0x11u8; 32]);
    let c_chain_id = Id::from([0x22u8; 32]);
    let critical = [PLATFORM_CHAIN_ID, x_chain_id, c_chain_id]
        .into_iter()
        .collect();
    let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));

    // Step 26 queues the platform chain (Go: the P-Chain genesis specifies the
    // others). Here we additionally queue an X-Chain (with a synthetic genesis
    // `ava_avm` parses: 32-byte stop-vertex id + 8-byte BE Unix timestamp) and a
    // C-Chain to prove the per-`vm_id` dispatch + the honest C-Chain skip.
    init_chains(&manager, &genesis_bytes).expect("queue the platform chain");
    let mut x_genesis = vec![0x42u8; 32]; // arbitrary stop-vertex id.
    x_genesis.extend_from_slice(&0u64.to_be_bytes()); // genesis timestamp = epoch.
    manager
        .start_chain_creator(ChainParameters {
            id: x_chain_id,
            subnet_id: PRIMARY_NETWORK_ID,
            genesis_data: x_genesis,
            vm_id: avm_id(),
            fx_ids: Vec::new(),
            custom_beacons: Vec::new(),
        })
        .expect("queue the X-Chain");
    manager
        .start_chain_creator(ChainParameters {
            id: c_chain_id,
            subnet_id: PRIMARY_NETWORK_ID,
            genesis_data: Vec::new(),
            vm_id: evm_id(),
            fx_ids: Vec::new(),
            custom_beacons: Vec::new(),
        })
        .expect("queue the C-Chain");

    // Sanity: distinct, well-known VM ids.
    assert_ne!(platform_vm_id(), avm_id(), "P and X VM ids differ");
    assert_ne!(avm_id(), evm_id(), "X and C VM ids differ");

    let handles = run_queued_chains(&manager, network_id)
        .await
        .expect("the chain creator boots the queued P- and X-Chains");
    assert_eq!(handles.len(), 2, "P and X boot; C is skipped (M6.8)");
    assert_eq!(
        manager.running_chains(),
        2,
        "exactly the P- and X-Chains are registered as running"
    );

    // Poll until both P and X flip bootstrapped (solo ⇒ Bootstrapping → NormalOp).
    let mut p_flipped = false;
    let mut x_flipped = false;
    for _ in 0..400_000 {
        p_flipped |= manager.is_bootstrapped(PLATFORM_CHAIN_ID);
        x_flipped |= manager.is_bootstrapped(x_chain_id);
        if p_flipped && x_flipped {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(p_flipped, "info.isBootstrapped(P) flips true at NormalOp");
    assert!(x_flipped, "info.isBootstrapped(X) flips true at NormalOp");
    assert!(
        !manager.is_bootstrapped(c_chain_id),
        "info.isBootstrapped(C) stays honestly false (C-Chain skipped, M6.8)"
    );

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

    // A beaconless (solo) node: the creator drives the queued P-Chain to
    // NormalOp and `info.isBootstrapped(P)` flips true.
    {
        let bootstrappers: Arc<dyn ValidatorManager> = Arc::new(DefaultManager::new());
        let critical = std::iter::once(PLATFORM_CHAIN_ID).collect();
        let manager = Arc::new(AssemblyChainManager::new(critical, bootstrappers));
        init_chains(&manager, &genesis_bytes).expect("queue the platform chain");

        let handles = drive_startup_chains(&manager, network_id, /* beaconless = */ true)
            .await
            .expect("the creator drives the solo P-Chain");
        assert_eq!(handles.len(), 1, "exactly one solo P-Chain booted");
        assert_eq!(
            manager.running_chains(),
            1,
            "the booted P-Chain is registered as a running chain"
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
