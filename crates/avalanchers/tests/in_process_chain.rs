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
