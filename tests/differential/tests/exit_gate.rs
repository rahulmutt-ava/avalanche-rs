// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M8.32 milestone exit-gate aggregator.
//!
//! The M8 milestone is "green" only when the five named exit-gate tests exist
//! and are wired as real `#[test]`/`#[tokio::test]` functions in their owning
//! crates (plan/M8-node-config-api.md, Task M8.32 Step 1). Those tests each live
//! in a different crate, so a Rust test in any single crate cannot link them.
//! This aggregator instead asserts each named test SOURCE is present on disk and
//! still defines its canonical test function(s) under a test attribute — so the
//! gate fails loudly if an exit test is deleted or renamed out from under CI.
//!
//! The canonical registered nextest IDs (run them per-PR; the two `differential_*`
//! suites use the recorded-Go-oracle arm in CI) are:
//!
//! | gate | nextest id |
//! |------|------------|
//! | M8.4  flag_parity       | `ava-config::golden_flag_parity flag_parity`            |
//! | M8.8  genesis_block_id  | `ava-genesis::golden_genesis_block_id genesis_block_id` |
//! | M8.11 config_precedence | `ava-config::prop_config_precedence config_precedence`  |
//! | M8.23 api_parity        | `ava-api::differential_api_parity` (8-test suite)       |
//! | M8.24 indexer_parity    | `ava-indexer::differential_indexer_parity indexer_parity` |

// This integration target consumes none of the crate's own deps (it is a
// filesystem/source-presence gate), so it opts out of the per-binary
// `unused_crate_dependencies` false positive like the crate's other targets.
#![allow(unused_crate_dependencies)]

use std::path::PathBuf;

/// One named M8 exit-gate test, located by its owning crate + test source file.
struct ExitGate {
    /// Human label / milestone task.
    label: &'static str,
    /// Workspace-relative directory of the owning crate.
    crate_dir: &'static str,
    /// Crate-relative path to the integration-test source file.
    test_file: &'static str,
    /// Canonical test function(s) that MUST be present in that file. Each must
    /// appear as `fn <name>(` and the file must carry a `#[test]`/`#[tokio::test]`.
    required_fns: &'static [&'static str],
}

const EXIT_GATES: &[ExitGate] = &[
    ExitGate {
        label: "M8.4 golden::flag_parity",
        crate_dir: "crates/ava-config",
        test_file: "tests/golden_flag_parity.rs",
        required_fns: &["flag_parity"],
    },
    ExitGate {
        label: "M8.8 golden::genesis_block_id",
        crate_dir: "crates/ava-genesis",
        test_file: "tests/golden_genesis_block_id.rs",
        required_fns: &["genesis_block_id"],
    },
    ExitGate {
        label: "M8.11 prop::config_precedence",
        crate_dir: "crates/ava-config",
        test_file: "tests/prop_config_precedence.rs",
        required_fns: &["config_precedence"],
    },
    ExitGate {
        // The api_parity gate is the whole `differential_api_parity` suite; pin
        // the two load-bearing members: the reply-shape oracle (`info_parity`)
        // and the P/X method-set pin (`platform_and_avm_method_sets_pinned`).
        label: "M8.23 differential::api_parity",
        crate_dir: "crates/ava-api",
        test_file: "tests/differential_api_parity.rs",
        required_fns: &["info_parity", "platform_and_avm_method_sets_pinned"],
    },
    ExitGate {
        label: "M8.24 differential::indexer_parity",
        crate_dir: "crates/ava-indexer",
        test_file: "tests/differential_indexer_parity.rs",
        required_fns: &["indexer_parity"],
    },
];

/// Workspace root, derived from this crate's manifest dir (`<root>/tests/differential`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("ava-differential manifest dir has a <root>/tests/differential shape")
        .to_path_buf()
}

#[test]
fn all_m8_exit_gate_tests_registered() {
    let root = workspace_root();
    for gate in EXIT_GATES {
        let path = root.join(gate.crate_dir).join(gate.test_file);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "exit gate `{}`: source {} unreadable ({e}) — exit-gate test missing",
                gate.label,
                path.display(),
            )
        });
        assert!(
            src.contains("#[test]") || src.contains("#[tokio::test]"),
            "exit gate `{}`: {} carries no #[test]/#[tokio::test] attribute",
            gate.label,
            path.display(),
        );
        for func in gate.required_fns {
            let needle = format!("fn {func}(");
            assert!(
                src.contains(&needle),
                "exit gate `{}`: canonical test fn `{func}` missing from {}",
                gate.label,
                path.display(),
            );
        }
    }
}
