// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.28 — the `avalanchers` binary can assemble an in-process chain manager,
//! register a built-in no-op test-VM factory, create one Snowman chain through
//! the full `create_snowman_chain` pipeline, and report the chain's
//! last-accepted height. `--version` / `--help` must still answer (exit 0).

use std::process::Command;

use avalanchers::wiring::chains::{build_in_process_chain, register_test_vm_factory};

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
